//! Item, container and recipe-book packets: item component decode, slot
//! display, containers, merchant offers, advancements and stats. Split out
//! of the former monolithic `adapter.rs`.
use super::*;

impl V770Adapter {
    /// Clientbound play-state packets in the inventory domain, split out of the
    /// former monolithic `handle_play` (see `adapter::mod` for the coordinator).
    pub(super) fn handle_play_inventory(&self, packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == play::clientbound::CONTAINER_SET_CONTENT {
            let mut reader = Reader::new(payload);
            let window_id = reader.var_i32().map_err(dec_err)?;
            let state_id = reader.var_i32().map_err(dec_err)?;
            let len = reader.var_i32().map_err(dec_err)?;
            let len = usize::try_from(len)
                .map_err(|_| AdapterError::Decode(format!("invalid item count {len}")))?;
            let mut items = Vec::with_capacity(len);
            let mut complete = true;
            for _ in 0..len {
                match read_item_stack(&mut reader)? {
                    DecodedStack::Complete(stack) => items.push(stack),
                    // An unmodeled component ended the patch; the remaining list
                    // entries and the carried item are unreadable. Deliver what
                    // decoded and drop the rest of the packet.
                    DecodedStack::Partial(stack) => {
                        items.push(stack);
                        complete = false;
                        break;
                    }
                }
            }
            let carried_item = if complete {
                match read_item_stack(&mut reader)? {
                    DecodedStack::Complete(stack) => stack,
                    DecodedStack::Partial(stack) => {
                        complete = false;
                        stack
                    }
                }
            } else {
                None
            };
            if complete {
                reader.ensure_empty().map_err(dec_err)?;
            }
            return Ok(vec![Directive::Emit(ClientEvent::ContainerContent {
                window_id,
                state_id,
                items,
                carried_item,
            })]);
        }
        if packet_id == play::clientbound::CONTAINER_SET_SLOT {
            let mut reader = Reader::new(payload);
            let window_id = reader.var_i32().map_err(dec_err)?;
            let state_id = reader.var_i32().map_err(dec_err)?;
            let slot = i32::from(reader.i16().map_err(dec_err)?);
            let item = read_trailing_item_stack(&mut reader)?;
            return Ok(vec![Directive::Emit(ClientEvent::ContainerSlot {
                window_id,
                state_id,
                slot,
                item,
            })]);
        }
        if packet_id == play::clientbound::CONTAINER_SET_DATA {
            let mut reader = Reader::new(payload);
            let window_id = reader.var_i32().map_err(dec_err)?;
            let property = i32::from(reader.i16().map_err(dec_err)?);
            let value = i32::from(reader.i16().map_err(dec_err)?);
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ContainerData {
                window_id,
                property,
                value,
            })]);
        }
        if packet_id == play::clientbound::CONTAINER_CLOSE {
            let mut reader = Reader::new(payload);
            let window_id = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ScreenClosed {
                window_id,
            })]);
        }
        if packet_id == play::clientbound::RECIPE_BOOK_SETTINGS {
            // `RecipeBookSettings.STREAM_CODEC` composes four `TypeSettings`, each
            // two booleans, in the fixed order crafting, furnace, blast furnace,
            // smoker. Eight bytes, no length prefix and no discriminator — the
            // codec is `StreamCodec<FriendlyByteBuf, _>`, i.e. not registry-aware,
            // which is the structural proof that nothing else is on the wire.
            //
            // Field order within a pair is `open` then `filtering`. Getting that
            // pair backwards is the available mistake here and it is invisible to a
            // round-trip test, so `recipe_book_settings_wire_order_is_open_then_filtering`
            // pins it against a hand-built asymmetric byte pattern.
            let mut reader = Reader::new(payload);
            let mut settings = [RecipeBookTypeSettings::default(); 4];
            for slot in &mut settings {
                slot.open = reader.bool().map_err(dec_err)?;
                slot.filtering = reader.bool().map_err(dec_err)?;
            }
            reader.ensure_empty().map_err(dec_err)?;
            let [crafting, furnace, blast_furnace, smoker] = settings;
            return Ok(vec![Directive::Emit(
                ClientEvent::RecipeBookSettingsChanged {
                    crafting,
                    furnace,
                    blast_furnace,
                    smoker,
                },
            )]);
        }
        if packet_id == play::clientbound::SET_CURSOR_ITEM {
            let mut reader = Reader::new(payload);
            let item = read_trailing_item_stack(&mut reader)?;
            return Ok(vec![Directive::Emit(ClientEvent::CursorItemChanged {
                item,
            })]);
        }
        if packet_id == play::clientbound::SET_PLAYER_INVENTORY {
            let mut reader = Reader::new(payload);
            let slot = reader.var_i32().map_err(dec_err)?;
            let item = read_trailing_item_stack(&mut reader)?;
            return Ok(vec![Directive::Emit(ClientEvent::InventorySlotChanged {
                slot,
                item,
            })]);
        }
        if packet_id == play::clientbound::OPEN_SCREEN {
            return decode_open_screen(payload);
        }
        if packet_id == play::clientbound::MAP_ITEM_DATA {
            return decode_map_item_data(payload);
        }
        if packet_id == play::clientbound::UPDATE_ADVANCEMENTS {
            return decode_update_advancements(payload);
        }
        // ---- issue #26: the remaining clientbound set ----------------------
        //
        // Every layout below was read off the record definition in
        // `.cache/mc/26.2/src`. Where a payload is carried as opaque bytes the
        // reason is stated at the decoder, and it is always the same reason: the
        // value is a *schema* (an NBT `Codec` union, or a per-registry-entry
        // codec table) rather than a `StreamCodec`, so decoding it is a
        // renderer's problem and not the wire's.
        if packet_id == play::clientbound::AWARD_STATS {
            return decode_award_stats(payload);
        }
        if packet_id == play::clientbound::SHOW_DIALOG {
            return decode_show_dialog(payload);
        }
        if packet_id == play::clientbound::CLEAR_DIALOG {
            Reader::new(payload).ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::DialogCleared)]);
        }
        if packet_id == play::clientbound::RECIPE_BOOK_REMOVE {
            let mut reader = Reader::new(payload);
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count).map_err(|_| {
                AdapterError::Decode(format!("invalid recipe_book_remove count {count}"))
            })?;
            let mut display_ids = Vec::with_capacity(count.min(4096));
            for _ in 0..count {
                display_ids.push(reader.var_i32().map_err(dec_err)?);
            }
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::RecipeBookRemoved {
                display_ids,
            })]);
        }
        if packet_id == play::clientbound::RECIPE_BOOK_ADD {
            return decode_recipe_book_add(payload);
        }
        if packet_id == play::clientbound::PLACE_GHOST_RECIPE {
            let mut reader = Reader::new(payload);
            let window_id = reader.var_i32().map_err(dec_err)?;
            let Some(result_items) = read_recipe_display(&mut reader)? else {
                // An unmodeled nested display: the reader's position is no longer
                // trustworthy, so drop the packet rather than emit a half-read
                // event. Same contract as `read_component_patch`'s bail-out.
                return Ok(Vec::new());
            };
            return Ok(vec![Directive::Emit(ClientEvent::GhostRecipeShown {
                window_id,
                result_items,
            })]);
        }
        if packet_id == play::clientbound::UPDATE_RECIPES {
            return decode_update_recipes(payload);
        }
        if packet_id == play::clientbound::MERCHANT_OFFERS {
            return decode_merchant_offers(payload);
        }
        Ok(Vec::new())
    }
}

/// Outcome of decoding one clientbound item stack.
///
/// # Why this is an enum and not a `{ stack, complete }` struct
///
/// It used to be a struct with a `complete: bool`, and a caller
/// (`decode_merchant_offers`) wrote `read_item_stack(reader)?.stack` — dropping
/// the flag and reading the *next* offer out of a reader parked mid-payload.
/// Every field after that decoded as a plausible-but-wrong value. A `bool`
/// beside the thing you actually want is an affordance to ignore it; an enum
/// has none, because there is no way to reach the stack without naming which
/// case you are in. **Do not reintroduce an accessor that returns the stack
/// without the verdict** (no `fn stack(self) -> Option<ItemStack>`), or the
/// affordance comes straight back.
///
/// The patch codec length-prefixes neither the patch nor its individual
/// components (26.2 `DataComponentPatch.STREAM_CODEC`, the undelimited variant
/// clientbound stacks use), so an unrecognised component cannot be skipped in
/// place — hence a partial outcome at all. See [`read_item_stack`].
#[must_use]
pub(crate) enum DecodedStack {
    /// The stack was consumed exactly; the reader sits immediately after it and
    /// reading on is safe. Inner `None` is the empty stack.
    Complete(Option<ItemStack>),
    /// An unmodeled component halted decoding partway through the stack's
    /// `DataComponentPatch`. The modeled fields that were decoded are valid, but
    /// **the rest of this packet is gone**: emit what is here and stop.
    ///
    /// The reader has been drained to its end by [`read_component_patch`], so a
    /// caller that ignores this and reads on gets a clean `UnexpectedEof` — a
    /// dropped packet, which the client driver survives — instead of silently
    /// consuming payload bytes as ids and lengths.
    Partial(Option<ItemStack>),
}

/// Decodes a clientbound optional item stack.
///
/// Wire shape (26.2 `ItemStack.OPTIONAL_STREAM_CODEC`): a VarInt count — `<= 0`
/// means the empty stack — then the item registry id as a VarInt, then a
/// `DataComponentPatch` (a VarInt count of added components and a VarInt count of
/// removed components; both zero means an empty patch). Each added component is a
/// `(type id VarInt, payload)` pair and each removed component a bare type id.
///
/// Added component payloads are **not** length-prefixed in the clientbound
/// (trusted) codec, so a component this build does not model cannot be skipped.
/// Rather than tear down the session on the next unrecognised component — every
/// future component addition would then be an outage — decoding degrades: the
/// modeled components (custom name, damage, enchantments) are decoded, and the
/// first unmodeled component stops the patch, flags the stack as partial
/// ([`ItemComponents::has_unmodeled`]), and yields it with `complete == false`.
///
/// `pub(crate)` because entity metadata carries the *same* codec under its
/// `ITEM_STACK` serializer (a dropped item's whole identity is one such field).
/// That path must reuse this decoder rather than grow a second one — two
/// independent readings of the component-patch wire is exactly how the two ends
/// drift apart.
pub(crate) fn read_item_stack(reader: &mut Reader<'_>) -> Result<DecodedStack, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    if count <= 0 {
        return Ok(DecodedStack::Complete(None));
    }
    let item_id = reader.var_i32().map_err(dec_err)?;
    let name = item_name(item_id)
        .ok_or_else(|| AdapterError::Decode(format!("unknown item registry id {item_id}")))?;
    let count = u32::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid item count {count}")))?;
    let (components, complete) = read_component_patch(reader, name)?;
    let stack = Some(ItemStack {
        item: parse_key(name, "item")?,
        count,
        components,
    });
    Ok(if complete {
        DecodedStack::Complete(stack)
    } else {
        DecodedStack::Partial(stack)
    })
}

/// `minecraft:trim_material` registry paths in **vanilla bootstrap order**
/// (`TrimMaterials.bootstrap`, `TrimMaterials.java:25-35`), which is the order a
/// vanilla server's Configuration-phase registry sync assigns ids in.
///
/// # Why this table and not a synced registry
///
/// `Registries.TRIM_MATERIAL` is a **dynamic** registry: its ids come from the
/// `registry_data` packets sent during Configuration, and this client keeps no
/// dynamic-registry store, so a `Holder::REFERENCE` id has nothing to resolve
/// against. Bootstrap order is what a vanilla server without a trim datapack
/// sends, so this is exact for vanilla and **provisional** for a modded server —
/// the same posture, and the same caveat, as `server_protocol.rs`'s `BIOME_NAMES`.
/// An id outside the table decodes as the empty string rather than failing: the
/// bytes are consumed either way, which is the property that keeps the rest of the
/// packet readable.
///
/// Deliberately *not* read from `lodestone_assets::trim::TRIM_MATERIALS`, which
/// happens to be in this same order today — `TRIM_PATTERNS` beside it is
/// alphabetical, so "the asset table is in registry order" is a coincidence for
/// one of the two and cannot be relied on for either.
const TRIM_MATERIAL_IDS: &[&str] = &[
    "quartz",
    "iron",
    "netherite",
    "redstone",
    "copper",
    "gold",
    "emerald",
    "diamond",
    "lapis",
    "amethyst",
    "resin",
];
/// `minecraft:trim_pattern` registry paths in vanilla bootstrap order
/// (`TrimPatterns.bootstrap`, `TrimPatterns.java:31-48`). See
/// [`TRIM_MATERIAL_IDS`] for the id-space caveat — and note this is **not** the
/// alphabetical order `lodestone_assets::trim::TRIM_PATTERNS` uses.
const TRIM_PATTERN_IDS: &[&str] = &[
    "sentry",
    "dune",
    "coast",
    "wild",
    "ward",
    "eye",
    "vex",
    "tide",
    "snout",
    "rib",
    "spire",
    "wayfinder",
    "shaper",
    "silence",
    "raiser",
    "host",
    "flow",
    "bolt",
];
/// Decodes `minecraft:trim`'s payload — `ArmorTrim.STREAM_CODEC`
/// (`ArmorTrim.java:26-28`), a `Holder<TrimMaterial>` then a
/// `Holder<TrimPattern>`.
///
/// Each holder is `ByteBufCodecs.holder(registry, DIRECT_STREAM_CODEC)`: a VarInt
/// where `0` introduces an **inline** definition and any positive value references
/// the registry at `value - 1`. Both forms are handled, because both must be — the
/// inline form is what a datapack-defined trim arrives as, and consuming the wrong
/// number of bytes for it would desync the rest of the packet exactly as the
/// unmodeled-component cliff this arm exists to remove does.
///
/// The inline bodies, from the two `DIRECT_STREAM_CODEC`s:
///
/// * `TrimMaterial` (`TrimMaterial.java:22-24`) — a `MaterialAssetGroup` (an
///   `AssetInfo` = one UTF-8 string, then a map of `ResourceKey -> AssetInfo`,
///   i.e. a VarInt count of `(string, string)` pairs) then a description
///   `Component` (network NBT).
/// * `TrimPattern` (`TrimPattern.java:25-33`) — an `Identifier` (string), a
///   description `Component`, then a `bool` `decal`.
///
/// **The inline material carries no registry name**, only its asset suffix, so
/// that is what is reported: for every vanilla material the suffix *is* the
/// registry path (`MaterialAssetGroup::create(base)`, `MaterialAssetGroup.java:36-46`),
/// and it is also the half `lodestone_assets::trim::trim_sprite_id` actually needs.
fn read_armor_trim(reader: &mut Reader<'_>) -> Result<ArmorTrim, AdapterError> {
    let material = match reader.var_i32().map_err(dec_err)? {
        0 => {
            let base = reader.string(32767).map_err(dec_err)?;
            let overrides = reader.var_i32().map_err(dec_err)?;
            for _ in 0..overrides {
                let _key = reader.string(32767).map_err(dec_err)?;
                let _suffix = reader.string(32767).map_err(dec_err)?;
            }
            let _description = read_network_nbt(reader).map_err(dec_err)?;
            base
        }
        holder => TRIM_MATERIAL_IDS
            .get((holder - 1) as usize)
            .copied()
            .unwrap_or_default()
            .to_owned(),
    };
    let pattern = match reader.var_i32().map_err(dec_err)? {
        0 => {
            let asset_id = reader.string(32767).map_err(dec_err)?;
            let _description = read_network_nbt(reader).map_err(dec_err)?;
            let _decal = reader.bool().map_err(dec_err)?;
            // The asset id is a full identifier; the registry path is what the
            // asset layer keys by.
            asset_id
                .rsplit_once(':')
                .map_or(asset_id.clone(), |(_, path)| path.to_owned())
        }
        holder => TRIM_PATTERN_IDS
            .get((holder - 1) as usize)
            .copied()
            .unwrap_or_default()
            .to_owned(),
    };
    Ok(ArmorTrim { material, pattern })
}

/// Decodes `minecraft:pot_decorations`' payload — `PotDecorations.STREAM_CODEC`,
/// which is `ByteBufCodecs.registry(Registries.ITEM).apply(ByteBufCodecs.list(4))`.
///
/// So the wire is a VarInt element count (`ByteBufCodecs.readCount`, capped at 4)
/// followed by that many **bare** item registry ids as VarInts. Two shapes it is
/// easy to get wrong, both re-read from the jar rather than inferred:
///
/// * `ByteBufCodecs.registry` is `idMapper`, which writes `VarInt.write(id)` with
///   **no `+1` and no `0` sentinel** — unlike `ByteBufCodecs.holder`, which
///   `minecraft:trim` uses two arms above. Adding an offset here would consume the
///   right number of bytes and report the wrong four sherds.
/// * The list is `list(4)`, a *maximum*, not a fixed width. A vanilla server
///   always writes four (`PotDecorations::ordered` builds a four-element list
///   unconditionally), but a shorter list is legal on the wire and its missing
///   tail is `Optional.empty()` — `PotDecorations::getItem`'s `i >= sherds.size()`
///   arm.
///
/// `minecraft:brick` decodes to `None`, mirroring `getItem`'s
/// `item == Items.BRICK ? Optional.empty() : Optional.of(item)`. An id outside the
/// item registry decodes as `None` rather than failing, for the same reason
/// [`TRIM_MATERIAL_IDS`] tolerates an unknown holder: the bytes are consumed
/// either way, and that is the property keeping the rest of the packet readable.
fn read_pot_decorations(reader: &mut Reader<'_>) -> Result<PotDecorations, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    if !(0..=4).contains(&count) {
        return Err(AdapterError::Decode(format!(
            "pot_decorations declares {count} sherds; ByteBufCodecs.list(4) permits 0..=4"
        )));
    }
    let mut sides: [Option<ResourceKey>; 4] = [None, None, None, None];
    for side in sides.iter_mut().take(count as usize) {
        let id = reader.var_i32().map_err(dec_err)?;
        // A brick face and an absent face are the same state in vanilla, so both
        // land on `None`.
        *side = match item_name(id) {
            Some("minecraft:brick") | None => None,
            Some(name) => Some(parse_key(name, "pot decoration")?),
        };
    }
    let [back, left, right, front] = sides;
    Ok(PotDecorations {
        back,
        left,
        right,
        front,
    })
}

/// Decodes an item stack's `DataComponentPatch` into the modeled component set,
/// returning whether the patch was fully consumed.
///
/// Modeled added components are read into their fields; the first unmodeled
/// added component stops decoding (its payload is not length-prefixed and so
/// cannot be skipped), flags the set, and returns `complete == false`. Removed
/// components are bare type ids and are always skippable, so a patch that
/// reaches them is fully consumed.
///
/// # The three *effective* fields start from the item's prototype
///
/// `max_stack_size`, `max_damage` and `equippable` are **not** patch fields —
/// they are the item's built-in prototype values, folded with whatever the patch
/// says. They are seeded here from [`lodestone_data::item_prototypes`] *before* the patch
/// is read, because a clientbound patch almost never mentions any of them
/// (vanilla keeps all three in the prototype component map) and a stack that
/// reported "unknown" for them would leave armour unequippable and every stack
/// cap at 64. See [`ItemComponents`] for why they are effective rather than
/// patch-shaped, and `docs/item-prototypes.md` for the census.
fn read_component_patch(
    reader: &mut Reader<'_>,
    item: &str,
) -> Result<(ItemComponents, bool), AdapterError> {
    let added = reader.var_i32().map_err(dec_err)?;
    let removed = reader.var_i32().map_err(dec_err)?;
    let mut components = ItemComponents::default();
    if let Some(prototype) = lodestone_data::item_prototypes::prototype(item) {
        components.max_stack_size = Some(u32::from(prototype.max_stack_size));
        components.max_damage = prototype.max_damage.map(u32::from);
        components.equippable = prototype.equip_slot;
    }

    for _ in 0..added {
        let type_id = reader.var_i32().map_err(dec_err)?;
        match component_type_name(type_id) {
            Some("minecraft:custom_name") => {
                let nbt = read_network_nbt(reader).map_err(dec_err)?;
                components.custom_name = Some(Text::from_nbt(&nbt));
            }
            Some("minecraft:damage") => {
                let damage = reader.var_i32().map_err(dec_err)?;
                components.damage = Some(u32::try_from(damage).map_err(|_| {
                    AdapterError::Decode(format!("negative item damage {damage}"))
                })?);
            }
            Some("minecraft:enchantments") => {
                components.enchantments = read_enchantments(reader)?;
            }
            Some("minecraft:tool") => {
                components.tool = ToolPatch::Set(read_tool(reader)?);
            }
            // `DyedItemColor.STREAM_CODEC` is a bare `ByteBufCodecs.INT`
            // (`DyedItemColor.java:24`) — fixed-width, not a `VarInt` like
            // every other scalar component here, so this is the one `i32()`
            // read in this match rather than `var_i32()`.
            Some("minecraft:dyed_color") => {
                components.dyed_color = Some(reader.i32().map_err(dec_err)? as u32);
            }
            // Decoded rather than left unmodeled *because* the `other` arm below
            // cannot skip: a trimmed armour stack used to truncate the whole
            // remaining packet, not merely lose its trim. See [`read_armor_trim`].
            Some("minecraft:trim") => {
                components.trim = Some(read_armor_trim(reader)?);
            }
            // `MapId.STREAM_CODEC` is `ByteBufCodecs.VAR_INT.map(MapId::new, …)`
            // (`MapId.java:19`), registered `networkSynchronized` at
            // `DataComponents.java:229`. Decoded for the same reason as the trim
            // above — a filled map in any inventory was truncating the packet from
            // here on, not merely losing which map it showed.
            Some("minecraft:map_id") => {
                components.map_id = Some(reader.var_i32().map_err(dec_err)?);
            }
            // Decoded for the same reason as the trim and the map id above, and
            // this one was found the hard way: the vanilla advancement
            // `adventure/craft_decorated_pot_using_only_sherds` has a
            // `minecraft:decorated_pot` icon, so a server that has sent any
            // advancement tree at all truncates `update_advancements` here — a
            // **join-blocking** failure, since that packet lands during the
            // initial world load. See [`read_pot_decorations`].
            Some("minecraft:pot_decorations") => {
                components.pot_decorations = Some(read_pot_decorations(reader)?);
            }
            // Both of these are `ByteBufCodecs.VAR_INT` (`DataComponents.java:110-115`)
            // and both *override* the prototype value seeded above. They are
            // decoded rather than treated as unmodeled not because servers send
            // them often — they essentially never do — but because a patch that
            // did carry one would otherwise stop decoding here and leave the
            // seeded prototype value silently stale.
            Some("minecraft:max_stack_size") => {
                let size = reader.var_i32().map_err(dec_err)?;
                components.max_stack_size = Some(u32::try_from(size).map_err(|_| {
                    AdapterError::Decode(format!("negative item max_stack_size {size}"))
                })?);
            }
            Some("minecraft:max_damage") => {
                let max = reader.var_i32().map_err(dec_err)?;
                components.max_damage = Some(u32::try_from(max).map_err(|_| {
                    AdapterError::Decode(format!("negative item max_damage {max}"))
                })?);
            }

            // ---------------------------------------------------------------
            // Consumed-for-alignment components.
            //
            // Everything from here to the `other` arm is decoded for exactly one
            // reason: an unmodeled component ends the packet. Nothing below is
            // *used* by this client (only `custom_data` is even kept), and that
            // is the point — the value is worthless and consuming the right
            // number of bytes is worth a whole packet. Each arm cites the vanilla
            // stream codec it mirrors; get a width wrong here and the failure is
            // silent misalignment rather than an honest bail-out, so no arm is
            // added without reading its codec in the jar.
            // ---------------------------------------------------------------

            // **The derived-NBT family.** These components are registered with
            // `persistent(codec)` and **no** `networkSynchronized(...)`, so
            // `DataComponentType.Builder.build` falls back to
            // `ByteBufCodecs.fromCodecWithRegistries(codec)` — which writes the
            // value as a single `FriendlyByteBuf.writeNbt` tag (root tag id then
            // payload, no name, no length prefix). One rule covers all seven, and
            // it is *not* the same codec as `CustomData.STREAM_CODEC`, which is
            // `@Deprecated` and used by `bucket_entity_data` rather than by
            // `custom_data`. Reading either as a bare compound would be wrong for
            // `recipes` (a list tag) and for the `Unit`-valued one (an empty
            // compound from `MapCodec.unitCodec`).
            //
            // `custom_data` is the one worth singling out: it is component id 0,
            // it is what every Bukkit/Paper plugin stamps on a GUI item, and while
            // it was unmodeled a lobby hotbar truncated whatever packet carried
            // it. Its bytes are kept verbatim rather than interpreted — see
            // [`ItemComponents::custom_data`].
            Some("minecraft:custom_data") => {
                components.custom_data = Some(read_network_nbt_bytes(reader)?);
            }
            Some(
                "minecraft:intangible_projectile"
                | "minecraft:map_decorations"
                | "minecraft:debug_stick_state"
                | "minecraft:recipes"
                | "minecraft:lock"
                | "minecraft:container_loot",
            ) => {
                read_network_nbt(reader).map_err(dec_err)?;
            }

            // `Unit.STREAM_CODEC` is `StreamCodec.unit(INSTANCE)`: **zero bytes**.
            // The component's presence in the patch is the whole value.
            Some(
                "minecraft:unbreakable" | "minecraft:creative_slot_lock" | "minecraft:glider",
            ) => {}

            // A bare VarInt. `rarity`, `dye`, `base_color` and `map_post_processing`
            // are `ByteBufCodecs.idMapper`, which is `VarInt.read` with no `+1` and
            // no `0` sentinel; the rest are `ByteBufCodecs.VAR_INT` directly, or a
            // one-field `StreamCodec.composite` over it (`enchantable`,
            // `ominous_bottle_amplifier`).
            Some(
                "minecraft:rarity"
                | "minecraft:repair_cost"
                | "minecraft:additional_trade_cost"
                | "minecraft:ominous_bottle_amplifier"
                | "minecraft:enchantable"
                | "minecraft:dye"
                | "minecraft:base_color"
                | "minecraft:map_post_processing",
            ) => {
                reader.var_i32().map_err(dec_err)?;
            }

            // Fixed-width scalars, **not** VarInts. `MapItemColor.STREAM_CODEC` is
            // `ByteBufCodecs.INT` (the same trap `minecraft:dyed_color` documents
            // above), and the two floats are `ByteBufCodecs.FLOAT`.
            Some("minecraft:map_color") => {
                reader.i32().map_err(dec_err)?;
            }
            Some("minecraft:minimum_attack_charge" | "minecraft:potion_duration_scale") => {
                reader.f32().map_err(dec_err)?;
            }
            Some("minecraft:enchantment_glint_override") => {
                reader.bool().map_err(dec_err)?;
            }

            // `Identifier.STREAM_CODEC` is `ByteBufCodecs.STRING_UTF8.map(...)`:
            // one length-prefixed string, capped at 32767.
            Some(
                "minecraft:item_model" | "minecraft:tooltip_style" | "minecraft:note_block_sound",
            ) => {
                reader.string(32767).map_err(dec_err)?;
            }

            // `ComponentSerialization.STREAM_CODEC` — the same network-NBT chat
            // component `minecraft:custom_name` uses. `item_name` is the *item's*
            // name rather than a rename, so it is consumed and not surfaced;
            // nothing here prefers it over `custom_name`.
            Some("minecraft:item_name") => {
                read_network_nbt(reader).map_err(dec_err)?;
            }

            // `ItemLore.STREAM_CODEC` is `ComponentSerialization.STREAM_CODEC
            // .apply(ByteBufCodecs.list(256))`: a VarInt count then that many
            // network-NBT components. 256 is the codec's own cap.
            Some("minecraft:lore") => {
                let lines = read_count(reader, "lore line")?;
                if lines > 256 {
                    return Err(AdapterError::Decode(format!(
                        "lore declares {lines} lines; ByteBufCodecs.list(256) permits at most 256"
                    )));
                }
                for _ in 0..lines {
                    read_network_nbt(reader).map_err(dec_err)?;
                }
            }

            // `stored_enchantments` shares `ItemEnchantments.STREAM_CODEC` with
            // `minecraft:enchantments`, so it reuses that reader — but it is an
            // enchanted *book*'s payload, not the stack's own effects, so it is
            // deliberately not merged into `components.enchantments`.
            Some("minecraft:stored_enchantments") => {
                read_enchantments(reader)?;
            }

            Some("minecraft:custom_model_data") => read_custom_model_data(reader)?,
            Some("minecraft:tooltip_display") => read_tooltip_display(reader)?,
            Some("minecraft:attribute_modifiers") => read_attribute_modifiers(reader)?,

            other => {
                // An unmodeled component: its payload is not length-prefixed, so
                // it and everything after it in this packet are unreadable. Keep
                // the modeled fields decoded so far, flag the stack, and stop —
                // the packet is dropped past this point, not fatal.
                //
                // **Skipping is genuinely impossible here, re-verified against the
                // jar rather than inherited from this comment.** 26.2 has two patch
                // codecs: `DataComponentPatch.STREAM_CODEC` writes each payload raw
                // and `DELIMITED_STREAM_CODEC` length-prefixes it
                // (`DataComponentPatch.java:62-76`). Clientbound stacks use
                // `ItemStack.OPTIONAL_STREAM_CODEC`, built on the **undelimited**
                // one; the delimited variant is `OPTIONAL_UNTRUSTED_STREAM_CODEC`,
                // i.e. serverbound only (`ItemStack.java:124-126`). So there is no
                // length to skip and no self-describing framing to walk. The only
                // way to stop a given component being a decode cliff is to model
                // it, which is what the `minecraft:trim` arm above does.
                //
                // One special case: if the component we cannot decode is
                // `minecraft:equippable` itself, the prototype slot seeded above
                // is *known* to be overridden, so report "unknown" rather than a
                // value we can see is wrong. (`Equippable`'s stream codec is an
                // eleven-field record with a `HolderSet<EntityType>`; decoding it
                // for the sake of a component no vanilla server patches is not
                // worth the surface.)
                if other == Some("minecraft:equippable") {
                    components.equippable = None;
                }
                components.has_unmodeled = true;
                tracing::warn!(
                    item,
                    component = other.unwrap_or("unknown"),
                    component_id = type_id,
                    "unmodeled item data component; delivering a partial stack and \
                     skipping the rest of the packet",
                );
                // Park the reader at the end of the payload. Every caller is
                // *supposed* to stop on the `false` below, but one did not, and
                // the bytes it then read as item ids and list lengths were the
                // interior of this component's payload — plausible-but-wrong
                // values, or an over-read blamed on framing. Draining makes the
                // contract self-enforcing: the worst a caller that reads on can
                // now do is raise `UnexpectedEof`, i.e. drop the packet, which
                // is the outcome the design already promises. It also makes a
                // trailing-bytes assertion pass instead of firing spuriously.
                let _ = reader.bytes(reader.remaining());
                return Ok((components, false));
            }
        }
    }

    for _ in 0..removed {
        // Removed components carry only their type id (no payload) and clear a
        // component back to *nothing* — not to the item's prototype value. That
        // distinction only matters for a component whose prototype value we
        // actually use, which today is `minecraft:tool`: `/give …[!minecraft:tool]`
        // makes a pickaxe mine like a fist, and treating the removal as "no
        // opinion" would leave it at 8x. Every other modeled field defaults to
        // "absent" anyway, so consuming the id is enough for those.
        let type_id = reader.var_i32().map_err(dec_err)?;
        match component_type_name(type_id) {
            Some("minecraft:tool") => components.tool = ToolPatch::Removed,
            // A removal clears the component back to *nothing*, and vanilla's
            // own fallback with no `minecraft:max_stack_size` at all is **1**,
            // not 64 (`ItemInstance.java:14-16`) — so this is a real, if exotic,
            // way to make an item unstackable.
            Some("minecraft:max_stack_size") => components.max_stack_size = Some(1),
            // No `minecraft:max_damage` means not damageable, which is exactly
            // what `None` means here.
            Some("minecraft:max_damage") => components.max_damage = None,
            Some("minecraft:equippable") => components.equippable = None,
            _ => {}
        }
    }

    Ok((components, true))
}

/// Reads one network-NBT tag and returns the exact bytes it occupied.
///
/// Used for `minecraft:custom_data`, whose value this client deliberately does
/// not interpret: the bytes are re-emittable and float-free as far as `Eq` is
/// concerned, where a parsed `Nbt` would not be. The span is derived from the
/// reader's own cursor movement rather than re-serialised, so it is byte-exact
/// even for shapes our writer would normalise.
fn read_network_nbt_bytes(reader: &mut Reader<'_>) -> Result<Vec<u8>, AdapterError> {
    let before = reader.remaining_bytes();
    read_network_nbt(reader).map_err(dec_err)?;
    let consumed = before.len() - reader.remaining();
    Ok(before[..consumed].to_vec())
}

/// Consumes a `minecraft:custom_model_data` payload
/// (`CustomModelData.STREAM_CODEC`).
///
/// Four independent VarInt-counted lists, in order: floats, flags (bools),
/// strings, colours. **The colours are `ByteBufCodecs.INT`** — fixed-width
/// big-endian, not VarInts — which is the one width in this component that a
/// VarInt-by-default reader gets wrong, and getting it wrong misaligns the whole
/// rest of the packet instead of merely losing a colour.
fn read_custom_model_data(reader: &mut Reader<'_>) -> Result<(), AdapterError> {
    let floats = read_count(reader, "custom_model_data float")?;
    for _ in 0..floats {
        reader.f32().map_err(dec_err)?;
    }
    let flags = read_count(reader, "custom_model_data flag")?;
    for _ in 0..flags {
        reader.bool().map_err(dec_err)?;
    }
    let strings = read_count(reader, "custom_model_data string")?;
    for _ in 0..strings {
        reader.string(32767).map_err(dec_err)?;
    }
    let colors = read_count(reader, "custom_model_data color")?;
    for _ in 0..colors {
        reader.i32().map_err(dec_err)?;
    }
    Ok(())
}

/// Consumes a `minecraft:tooltip_display` payload (`TooltipDisplay.STREAM_CODEC`).
///
/// A bool `hideTooltip`, then a VarInt-counted collection of
/// `DataComponentType.STREAM_CODEC` — which is `ByteBufCodecs.registry`, i.e. a
/// bare data-component-type registry id per entry with no offset.
///
/// This component replaced 1.21.4's `minecraft:hide_tooltip` and
/// `hide_additional_tooltip`, and it is what a plugin sets to hide an item's
/// attribute lines — so it turns up on essentially every custom GUI item.
fn read_tooltip_display(reader: &mut Reader<'_>) -> Result<(), AdapterError> {
    reader.bool().map_err(dec_err)?;
    let hidden = read_count(reader, "tooltip_display hidden component")?;
    for _ in 0..hidden {
        reader.var_i32().map_err(dec_err)?;
    }
    Ok(())
}

/// Consumes a `minecraft:attribute_modifiers` payload
/// (`ItemAttributeModifiers.STREAM_CODEC`).
///
/// A VarInt-counted list of `Entry`, each of which is, in wire order:
///
/// * the attribute as `Attribute.STREAM_CODEC` = `ByteBufCodecs.holderRegistry`,
///   a **bare** VarInt registry id — `holderRegistry` is `registry(…,
///   Registry::asHolderIdMap)`, so unlike `ByteBufCodecs.holder` there is no `+1`
///   and no inline-holder `0` sentinel;
/// * the modifier as `AttributeModifier.STREAM_CODEC` — an `Identifier` string, a
///   **`ByteBufCodecs.DOUBLE`** (fixed-width f64, not a float), then the operation
///   as an idMapper VarInt;
/// * the slot group as `EquipmentSlotGroup.STREAM_CODEC`, an idMapper VarInt;
/// * the display as `Display.STREAM_CODEC`, a VarInt `Display.Type` id dispatching
///   to a payload: `default` (0) and `hidden` (1) are `StreamCodec.unit`, i.e.
///   **zero bytes**, and `override` (2) carries one network-NBT chat component.
///
/// The `display` field is the trap: it is new enough that a transcription from an
/// older `ItemAttributeModifiers` (which ended after the slot group, with a
/// trailing `showInTooltip` bool in 1.21.4 and earlier) reads one byte where two
/// of the three variants read one and the third reads a whole NBT blob.
fn read_attribute_modifiers(reader: &mut Reader<'_>) -> Result<(), AdapterError> {
    let entries = read_count(reader, "attribute modifier")?;
    for _ in 0..entries {
        reader.var_i32().map_err(dec_err)?; // Holder<Attribute>, bare id
        reader.string(32767).map_err(dec_err)?; // AttributeModifier::id
        reader.f64().map_err(dec_err)?; // amount
        reader.var_i32().map_err(dec_err)?; // Operation
        reader.var_i32().map_err(dec_err)?; // EquipmentSlotGroup
        let display = reader.var_i32().map_err(dec_err)?;
        match display {
            // `default` and `hidden` are `StreamCodec.unit`: no payload.
            0 | 1 => {}
            // `override` carries the replacement text.
            2 => {
                read_network_nbt(reader).map_err(dec_err)?;
            }
            other => {
                return Err(AdapterError::Decode(format!(
                    "attribute modifier display type {other} is outside \
                     ItemAttributeModifiers.Display.Type's 0..=2"
                )));
            }
        }
    }
    Ok(())
}

/// Decodes a `minecraft:tool` component (26.2 `Tool.STREAM_CODEC`).
///
/// Wire shape, in order: a VarInt-counted list of rules, then the default mining
/// speed as an f32, the damage-per-block as a VarInt, and the
/// can-destroy-in-creative flag as a bool. Each rule is a `HolderSet<Block>`,
/// then an optional f32 speed and an optional bool correct-for-drops (both
/// `ByteBufCodecs::optional`, so a present-flag byte then the value).
///
/// Note this component is *rarely* on the wire: a stack carries only the delta
/// from its item's prototype component map, and vanilla puts a pickaxe's
/// `minecraft:tool` in that prototype. It appears for `/give …[minecraft:tool={…}]`
/// and datapack-authored items. The prototype half lives in [`lodestone_data::tool`];
/// both feed the same evaluator.
fn read_tool(reader: &mut Reader<'_>) -> Result<ItemTool, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid tool rule count {count}")))?;
    let mut rules = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let blocks = read_block_holder_set(reader)?;
        let speed = if reader.bool().map_err(dec_err)? {
            Some(reader.f32().map_err(dec_err)?)
        } else {
            None
        };
        let correct_for_drops = if reader.bool().map_err(dec_err)? {
            Some(reader.bool().map_err(dec_err)?)
        } else {
            None
        };
        rules.push(ToolRule::new(blocks, speed, correct_for_drops));
    }
    let default_mining_speed = reader.f32().map_err(dec_err)?;
    let damage_per_block = reader.var_i32().map_err(dec_err)?;
    let damage_per_block = u32::try_from(damage_per_block).map_err(|_| {
        AdapterError::Decode(format!("negative tool damage_per_block {damage_per_block}"))
    })?;
    let can_destroy_blocks_in_creative = reader.bool().map_err(dec_err)?;
    Ok(ItemTool::new(
        rules,
        default_mining_speed,
        damage_per_block,
        can_destroy_blocks_in_creative,
    ))
}

/// Decodes a `HolderSet<Block>` (26.2 `ByteBufCodecs.holderSet(Registries.BLOCK)`).
///
/// A single VarInt discriminates: `0` means a named tag follows as an
/// identifier string; any `n > 0` means `n - 1` direct holders follow, each a
/// **bare** `minecraft:block` registry id.
///
/// # The direct holders are *not* `id + 1`
///
/// There are two holder codecs in 26.2 and they differ by exactly one:
/// `ByteBufCodecs.holder(key, directCodec)` reserves `0` for an inline element
/// definition and so writes `id + 1`, while `ByteBufCodecs.holderRegistry(key)`
/// — which is what `holderSet` uses internally — delegates to the private
/// `registry(key, Registry::asHolderIdMap)` and writes the id **as-is**. Only
/// the outer set-size discriminator is offset by one.
///
/// This was originally implemented as `id + 1` by reading the *first* codec and
/// assuming the second matched. The hermetic test agreed, because it encoded the
/// same way; the live capture in `tests/live_tool_component.rs` did not — the
/// real server wrote `minecraft:stone` (registry id 1) as `01` and
/// `minecraft:obsidian` (193) as `c1 01`, and we decoded them as 0 and 192.
fn read_block_holder_set(reader: &mut Reader<'_>) -> Result<ToolBlocks, AdapterError> {
    let discriminator = reader.var_i32().map_err(dec_err)?;
    if discriminator == 0 {
        // Vanilla's `Identifier.STREAM_CODEC` is an unbounded UTF-8 string, so
        // the limit here is the shared 32,767-char ceiling the rest of this
        // adapter uses, not a tighter guess that could reject a valid tag.
        let tag = reader.string(32767).map_err(dec_err)?;
        return Ok(ToolBlocks::Tag(parse_key(&tag, "block tag")?));
    }
    let count = usize::try_from(discriminator - 1)
        .map_err(|_| AdapterError::Decode(format!("invalid block set size {discriminator}")))?;
    let mut blocks = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let raw = reader.var_i32().map_err(dec_err)?;
        if raw < 0 {
            return Err(AdapterError::Decode(format!(
                "negative block registry id {raw} in a tool rule"
            )));
        }
        blocks.push(raw);
    }
    Ok(ToolBlocks::Blocks(blocks))
}

/// Decodes an `ItemEnchantments` component: a VarInt-counted map of
/// `Holder<Enchantment>` (registry id, holder-encoded as `id + 1`) to a VarInt
/// level.
fn read_enchantments(reader: &mut Reader<'_>) -> Result<Vec<ItemEnchantment>, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid enchantment count {count}")))?;
    let mut enchantments = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let raw = reader.var_i32().map_err(dec_err)?;
        if raw <= 0 {
            // 0 is an inline holder (a full Enchantment definition); vanilla
            // sends registry references for item enchantments, never inline.
            return Err(AdapterError::Decode(
                "inline enchantment holder is not supported".to_owned(),
            ));
        }
        let level = reader.var_i32().map_err(dec_err)?;
        let level = u32::try_from(level)
            .map_err(|_| AdapterError::Decode(format!("negative enchantment level {level}")))?;
        enchantments.push(ItemEnchantment {
            id: raw - 1,
            level,
        });
    }
    Ok(enchantments)
}

/// Reads an item stack that is the final field of a packet, asserting no
/// trailing bytes remain — unless an unmodeled component left the stack partial,
/// in which case the unread remainder is deliberately dropped rather than raised
/// as a fatal decode error.
fn read_trailing_item_stack(
    reader: &mut Reader<'_>,
) -> Result<Option<ItemStack>, AdapterError> {
    match read_item_stack(reader)? {
        DecodedStack::Complete(stack) => {
            reader.ensure_empty().map_err(dec_err)?;
            Ok(stack)
        }
        // The misparse detector is skipped deliberately: there are unread bytes
        // by construction. (They are also already drained, so `ensure_empty`
        // would pass — running it anyway would make this arm look load-bearing
        // when it is not, and would silently start failing if the drain ever
        // went away.)
        DecodedStack::Partial(stack) => Ok(stack),
    }
}

/// Decodes `open_screen`: a container id, a `minecraft:menu` registry id, and an
/// NBT text-component title.
fn decode_open_screen(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let window_id = reader.var_i32().map_err(dec_err)?;
    let menu_id = reader.var_i32().map_err(dec_err)?;
    let menu = menu_name(menu_id)
        .ok_or_else(|| AdapterError::Decode(format!("unknown menu id {menu_id}")))?;
    let menu_type = parse_key(menu, "menu")?;
    let title = read_network_nbt(&mut reader).map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::ScreenOpened {
        window_id,
        menu_type,
        title: Text::from_nbt(&title),
    })])
}

/// Registry ids of `minecraft:slot_display`, from `SlotDisplays.java`'s
/// registration order (and cross-checked against `registries.json`).
///
/// A **built-in** registry, so these ids are fixed by the jar rather than synced
/// during Configuration — the same reason `MAP_DECORATION_TYPE_IDS` is a table.
mod slot_display {
    pub const EMPTY: i32 = 0;
    pub const ANY_FUEL: i32 = 1;
    pub const WITH_ANY_POTION: i32 = 2;
    pub const ONLY_WITH_COMPONENT: i32 = 3;
    pub const ITEM: i32 = 4;
    pub const ITEM_STACK: i32 = 5;
    pub const TAG: i32 = 6;
    pub const DYED: i32 = 7;
    pub const SMITHING_TRIM: i32 = 8;
    pub const WITH_REMAINDER: i32 = 9;
    pub const COMPOSITE: i32 = 10;
}

/// What walking a `SlotDisplay` yielded.
///
/// `complete == false` means the walk hit something this adapter does not model
/// and **the reader's position is no longer trustworthy** — the caller must
/// abandon the whole packet rather than continue. Same convention as
/// [`read_component_patch`]'s second return value, and for the same reason: a
/// nested union with per-entry codecs cannot be skipped generically, so partial
/// progress is the honest outcome and silently continuing would misread every
/// following field.
#[derive(Debug, Default)]
struct SlotDisplayItems {
    /// Item registry ids this display can show, in encounter order.
    items: Vec<i32>,
    /// Whether the walk consumed the display exactly.
    complete: bool,
}

impl SlotDisplayItems {
    fn incomplete() -> Self {
        Self {
            items: Vec::new(),
            complete: false,
        }
    }
}

/// Walks one `SlotDisplay` (`SlotDisplay.STREAM_CODEC`), collecting the item ids
/// it can display.
///
/// # This is a byte-exact walk, not a skip
///
/// `SlotDisplay` is a **recursive** registry-dispatched union of eleven variants,
/// four of which contain further `SlotDisplay`s and one of which
/// (`composite`) contains a list of them. There is no length prefix anywhere, so
/// there is no way to skip one without decoding it — which is why every consumer
/// of `RecipeDisplay` in this crate had to wait for this function, and why the
/// five recipe packets landed together.
///
/// `depth` bounds the recursion: a malicious or corrupt payload could otherwise
/// nest `composite` indefinitely and blow the stack. Vanilla's own nesting is two
/// or three deep in practice.
fn read_slot_display(reader: &mut Reader<'_>, depth: u32) -> Result<SlotDisplayItems, AdapterError> {
    // 16 is far above vanilla's own two-or-three and well below anything that
    // threatens the stack. Returning `incomplete` rather than erroring keeps a
    // hostile payload a dropped packet instead of a disconnect.
    if depth > 16 {
        return Ok(SlotDisplayItems::incomplete());
    }
    let kind = reader.var_i32().map_err(dec_err)?;
    let mut items = Vec::new();
    match kind {
        slot_display::EMPTY | slot_display::ANY_FUEL => {}
        slot_display::ITEM => {
            items.push(reader.var_i32().map_err(dec_err)?);
        }
        slot_display::ITEM_STACK => {
            // `ItemStackTemplate.STREAM_CODEC`: item id, count, then a
            // `DataComponentPatch` — which is exactly what `read_component_patch`
            // walks, including its bail-out on an unmodeled component type.
            let item_id = reader.var_i32().map_err(dec_err)?;
            let _count = reader.var_i32().map_err(dec_err)?;
            let name = item_name(item_id).unwrap_or("minecraft:air");
            let (_components, complete) = read_component_patch(reader, name)?;
            if !complete {
                return Ok(SlotDisplayItems::incomplete());
            }
            items.push(item_id);
        }
        slot_display::TAG => {
            // `TagKey.streamCodec` is one `Identifier` string. The tag's *members*
            // are not on the wire, so there is no item id to collect — a consumer
            // that needs one resolves the tag itself.
            let _tag = reader.string(32767).map_err(dec_err)?;
        }
        slot_display::WITH_ANY_POTION => {
            let inner = read_slot_display(reader, depth + 1)?;
            if !inner.complete {
                return Ok(SlotDisplayItems::incomplete());
            }
            items.extend(inner.items);
        }
        slot_display::ONLY_WITH_COMPONENT => {
            let inner = read_slot_display(reader, depth + 1)?;
            if !inner.complete {
                return Ok(SlotDisplayItems::incomplete());
            }
            // `DataComponentType.STREAM_CODEC` is a bare VarInt registry id.
            let _component_type = reader.var_i32().map_err(dec_err)?;
            items.extend(inner.items);
        }
        slot_display::DYED | slot_display::WITH_REMAINDER => {
            // Two `SlotDisplay`s. For `dyed` they are (dye, target); for
            // `with_remainder` (input, remainder). Both halves are walked because
            // both must be consumed — only the first carries the item a recipe
            // panel wants, but skipping the second is not an option (no length
            // prefix).
            let first = read_slot_display(reader, depth + 1)?;
            if !first.complete {
                return Ok(SlotDisplayItems::incomplete());
            }
            let second = read_slot_display(reader, depth + 1)?;
            if !second.complete {
                return Ok(SlotDisplayItems::incomplete());
            }
            items.extend(first.items);
        }
        slot_display::SMITHING_TRIM => {
            for _ in 0..3 {
                let inner = read_slot_display(reader, depth + 1)?;
                if !inner.complete {
                    return Ok(SlotDisplayItems::incomplete());
                }
                items.extend(inner.items);
            }
            // `TrimPattern.STREAM_CODEC` is `ByteBufCodecs.holder`: `0` means an
            // inline `TrimPattern` follows, which this adapter does not model, so
            // that case abandons the packet rather than guessing at its length.
            let holder = reader.var_i32().map_err(dec_err)?;
            if holder == 0 {
                return Ok(SlotDisplayItems::incomplete());
            }
        }
        slot_display::COMPOSITE => {
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count).map_err(|_| {
                AdapterError::Decode(format!("invalid composite slot display count {count}"))
            })?;
            for _ in 0..count {
                let inner = read_slot_display(reader, depth + 1)?;
                if !inner.complete {
                    return Ok(SlotDisplayItems::incomplete());
                }
                items.extend(inner.items);
            }
        }
        // An id outside the built-in table means a modded registry entry whose
        // payload shape is unknown. The reader cannot go on.
        _ => return Ok(SlotDisplayItems::incomplete()),
    }
    Ok(SlotDisplayItems {
        items,
        complete: true,
    })
}

/// Walks one `RecipeDisplay` and returns the item ids of its **result** slot.
///
/// The result is what a recipe panel and a toast both key on; the ingredient
/// slots are walked only because they must be consumed. Returns `None` when the
/// walk hit something unmodeled, with the same "abandon the packet" contract as
/// [`read_slot_display`].
///
/// Variant ids are `RecipeDisplays.java`'s registration order: shapeless, shaped,
/// furnace, stonecutter, smithing.
fn read_recipe_display(reader: &mut Reader<'_>) -> Result<Option<Vec<i32>>, AdapterError> {
    let kind = reader.var_i32().map_err(dec_err)?;
    // Each variant is a fixed sequence of `SlotDisplay`s plus, for two of them,
    // some scalars. `result_index` is which of the walked displays is the result,
    // and `station_last` is true for every variant because `craftingStation` is
    // always the final `SlotDisplay`.
    let mut walked: Vec<Vec<i32>> = Vec::new();
    let walk = |reader: &mut Reader<'_>, walked: &mut Vec<Vec<i32>>| -> Result<bool, AdapterError> {
        let display = read_slot_display(reader, 0)?;
        if !display.complete {
            return Ok(false);
        }
        walked.push(display.items);
        Ok(true)
    };
    let result_index = match kind {
        // crafting_shapeless: list<SlotDisplay> ingredients, result, station.
        0 => {
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count).map_err(|_| {
                AdapterError::Decode(format!("invalid shapeless ingredient count {count}"))
            })?;
            for _ in 0..count {
                if !walk(reader, &mut walked)? {
                    return Ok(None);
                }
            }
            let ingredients = walked.len();
            for _ in 0..2 {
                if !walk(reader, &mut walked)? {
                    return Ok(None);
                }
            }
            ingredients
        }
        // crafting_shaped: width, height, list<SlotDisplay>, result, station.
        1 => {
            let _width = reader.var_i32().map_err(dec_err)?;
            let _height = reader.var_i32().map_err(dec_err)?;
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count).map_err(|_| {
                AdapterError::Decode(format!("invalid shaped ingredient count {count}"))
            })?;
            for _ in 0..count {
                if !walk(reader, &mut walked)? {
                    return Ok(None);
                }
            }
            let ingredients = walked.len();
            for _ in 0..2 {
                if !walk(reader, &mut walked)? {
                    return Ok(None);
                }
            }
            ingredients
        }
        // furnace: ingredient, fuel, result, station, duration, experience.
        2 => {
            for _ in 0..4 {
                if !walk(reader, &mut walked)? {
                    return Ok(None);
                }
            }
            let _duration = reader.var_i32().map_err(dec_err)?;
            let _experience = reader.f32().map_err(dec_err)?;
            2
        }
        // stonecutter: input, result, station.
        3 => {
            for _ in 0..3 {
                if !walk(reader, &mut walked)? {
                    return Ok(None);
                }
            }
            1
        }
        // smithing: template, base, addition, result, station.
        4 => {
            for _ in 0..5 {
                if !walk(reader, &mut walked)? {
                    return Ok(None);
                }
            }
            3
        }
        _ => return Ok(None),
    };
    Ok(walked.get(result_index).cloned().or(Some(Vec::new())))
}

/// Decodes `ClientboundAwardStatsPacket`: a VarInt-counted map of
/// `(stat_type id, value id) -> count`.
///
/// `Stat.STREAM_CODEC` is `registry(STAT_TYPE).dispatch(Stat::getType,
/// StatType::streamCodec)`, so the **second** id's registry depends on the first:
/// a value under `minecraft:mined` is a block, under `minecraft:killed` an entity
/// type, and under `minecraft:custom` one of the 77 custom stats. Resolving it
/// with one fixed table would silently mislabel every category but one.
///
/// An id this build cannot resolve yields `value: None` rather than an error — the
/// count is still correct and vanilla's own General tab is entirely
/// `minecraft:custom`, which we always resolve.
fn decode_award_stats(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    use crate::stat_debug_registries::{
        StatValueRegistry, custom_stat_name, stat_type_name, stat_value_registry,
    };

    let mut reader = Reader::new(payload);
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid award_stats count {count}")))?;
    let mut stats = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let type_id = reader.var_i32().map_err(dec_err)?;
        let value_id = reader.var_i32().map_err(dec_err)?;
        let stat_count = reader.var_i32().map_err(dec_err)?;
        let type_name = stat_type_name(type_id).ok_or_else(|| {
            AdapterError::Decode(format!("unknown stat_type registry id {type_id}"))
        })?;
        let value_name = match stat_value_registry(type_id) {
            Some(StatValueRegistry::CustomStat) => custom_stat_name(value_id),
            Some(StatValueRegistry::Item) => item_name(value_id),
            Some(StatValueRegistry::EntityType) => entity_type_name(value_id),
            // `block_type_name` indexes the `minecraft:block` *registry* (one id
            // per block type, registration order), which is what a `minecraft:mined`
            // stat value is — not a palette state id. `block_name` would be the
            // wrong table here and would resolve every id to an unrelated block.
            Some(StatValueRegistry::Block) => {
                u32::try_from(value_id).ok().and_then(block_type_name)
            }
            None => None,
        };
        stats.push(StatAward {
            stat_type: parse_key(type_name, "stat type")?,
            value: match value_name {
                Some(name) => Some(parse_key(name, "stat value")?),
                None => None,
            },
            count: stat_count,
        });
    }
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::StatisticsAwarded {
        stats,
    })])
}

/// Consumes a `HolderSet<Item>` (`Ingredient.CONTENTS_STREAM_CODEC`) and returns
/// the explicit item ids, or an empty list for the tag form.
///
/// Same wire shape as [`read_block_holder_set`], one registry over: a VarInt where
/// `0` means a tag identifier follows and `n` means `n - 1` explicit ids.
fn read_item_holder_set(reader: &mut Reader<'_>) -> Result<Vec<i32>, AdapterError> {
    let discriminator = reader.var_i32().map_err(dec_err)?;
    if discriminator == 0 {
        let _tag = reader.string(32767).map_err(dec_err)?;
        return Ok(Vec::new());
    }
    let count = usize::try_from(discriminator - 1)
        .map_err(|_| AdapterError::Decode(format!("invalid item set size {discriminator}")))?;
    let mut items = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        items.push(reader.var_i32().map_err(dec_err)?);
    }
    Ok(items)
}

/// Decodes `ClientboundRecipeBookAddPacket`.
///
/// **The trailing `replace: bool` sits after the entry list**, so the list cannot
/// be taken as opaque trailing bytes — the whole reason this packet waited for
/// [`read_slot_display`]. Each entry is a `RecipeDisplayEntry` then an `i8` flags
/// byte (bit 0 notification, bit 1 highlight).
///
/// `RecipeDisplayEntry`'s `group` field is `ByteBufCodecs.OPTIONAL_VAR_INT`: a
/// single VarInt where `0` is absent and a present value `v` is written `v + 1` —
/// **not** the usual bool-then-value optional. A bool-prefixed reader would
/// mis-frame every entry after the first.
fn decode_recipe_book_add(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid recipe_book_add count {count}")))?;
    let mut entries = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let display_id = reader.var_i32().map_err(dec_err)?;
        let Some(result_items) = read_recipe_display(&mut reader)? else {
            return Ok(Vec::new());
        };
        // `OPTIONAL_VAR_INT`, not a bool-prefixed optional.
        let _group = reader.var_i32().map_err(dec_err)?;
        let _category = reader.var_i32().map_err(dec_err)?;
        if reader.bool().map_err(dec_err)? {
            let requirement_count = reader.var_i32().map_err(dec_err)?;
            let requirement_count = usize::try_from(requirement_count).map_err(|_| {
                AdapterError::Decode(format!(
                    "invalid crafting requirement count {requirement_count}"
                ))
            })?;
            for _ in 0..requirement_count {
                let _ingredient = read_item_holder_set(&mut reader)?;
            }
        }
        let flags = reader.i8().map_err(dec_err)?;
        entries.push(RecipeBookEntry {
            display_id,
            result_items,
            notification: flags & 0x01 != 0,
            highlight: flags & 0x02 != 0,
        });
    }
    let replace = reader.bool().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::RecipeBookAdded {
        entries,
        replace,
    })])
}

/// Decodes `ClientboundUpdateRecipesPacket`: the property sets, then the
/// stonecutter list.
///
/// Despite the name this is **not** the recipe corpus — it is the per-slot "which
/// items are valid here" sets vanilla's screens grey out against, plus the
/// stonecutter's own input→result pairs. A `RecipePropertySet` is a VarInt-counted
/// list of item registry ids and needs no display walk; the stonecutter half does.
fn decode_update_recipes(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let set_count = reader.var_i32().map_err(dec_err)?;
    let set_count = usize::try_from(set_count)
        .map_err(|_| AdapterError::Decode(format!("invalid property set count {set_count}")))?;
    let mut item_sets = Vec::with_capacity(set_count.min(256));
    for _ in 0..set_count {
        let key = reader.string(32767).map_err(dec_err)?;
        let item_count = reader.var_i32().map_err(dec_err)?;
        let item_count = usize::try_from(item_count)
            .map_err(|_| AdapterError::Decode(format!("invalid property item count {item_count}")))?;
        let mut items = Vec::with_capacity(item_count.min(4096));
        for _ in 0..item_count {
            items.push(reader.var_i32().map_err(dec_err)?);
        }
        item_sets.push((parse_key(&key, "recipe property set")?, items));
    }
    let stonecutter_count = reader.var_i32().map_err(dec_err)?;
    let stonecutter_count = usize::try_from(stonecutter_count).map_err(|_| {
        AdapterError::Decode(format!("invalid stonecutter count {stonecutter_count}"))
    })?;
    let mut stonecutter_results = Vec::with_capacity(stonecutter_count.min(4096));
    for _ in 0..stonecutter_count {
        // `SingleInputEntry`: an `Ingredient` (HolderSet<Item>) then a
        // `SlotDisplay` — a bare display, not a whole `RecipeDisplay`.
        let _input = read_item_holder_set(&mut reader)?;
        let display = read_slot_display(&mut reader, 0)?;
        if !display.complete {
            // Emit what was decoded before the unmodeled entry rather than the
            // whole packet: the property sets above are complete and independently
            // useful, and they are the half a screen actually reads.
            return Ok(vec![Directive::Emit(
                ClientEvent::RecipePropertySetsUpdated {
                    item_sets,
                    stonecutter_results,
                },
            )]);
        }
        stonecutter_results.push(display.items);
    }
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(
        ClientEvent::RecipePropertySetsUpdated {
            item_sets,
            stonecutter_results,
        },
    )])
}

/// Decodes `ClientboundMerchantOffersPacket`.
///
/// # The two traps
///
/// **Five of `MerchantOffer`'s fields are big-endian `i32`s, not VarInts** —
/// `uses`, `maxUses`, `xp`, `specialPriceDiff` and `demand` are all `writeInt`.
/// Almost every other integer in this protocol is a VarInt, so a
/// VarInt-by-default reader gets all five wrong *and* desynchronises everything
/// after them.
///
/// **The trailing scalars come after the offer list.** `villagerLevel`,
/// `villagerXp`, `showProgress` and `canRestock` are all past the offers, so they
/// are unreachable without parsing every `MerchantOffer` — including each
/// `ItemCost`'s `DataComponentExactPredicate`, which is a VarInt-counted list of
/// typed components. That list is `EMPTY` for every vanilla trade; a non-empty one
/// is unmodeled here and abandons the packet rather than guessing at its length.
fn decode_merchant_offers(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let window_id = reader.var_i32().map_err(dec_err)?;
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid merchant offer count {count}")))?;
    let mut offers = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let Some(cost_a) = read_item_cost(&mut reader)? else {
            return Ok(Vec::new());
        };
        // **This was the bug.** It read `.stack` off the old struct and dropped
        // the completeness flag, so an offer whose result carried an unmodeled
        // component left the reader parked mid-payload and the loop went on to
        // read this offer's remaining eight fields — and then the *next* offer —
        // out of that component's interior. On a plugin server stamping
        // `minecraft:custom_data` on every trade result, that is one warning per
        // offer followed by an over-read blamed on framing. An offer list has no
        // per-entry length prefix and the trailing `villagerLevel`/`villagerXp`
        // scalars sit past it, so there is nothing to resynchronise to: the only
        // correct move is to abandon the packet, exactly as a non-empty
        // `DataComponentExactPredicate` does two lines up.
        let result = match read_item_stack(&mut reader)? {
            DecodedStack::Complete(stack) => stack,
            DecodedStack::Partial(_) => return Ok(Vec::new()),
        };
        let cost_b = if reader.bool().map_err(dec_err)? {
            match read_item_cost(&mut reader)? {
                Some(cost) => Some(cost),
                None => return Ok(Vec::new()),
            }
        } else {
            None
        };
        let out_of_stock = reader.bool().map_err(dec_err)?;
        // The five `writeInt` fields. Not VarInts.
        let uses = reader.i32().map_err(dec_err)?;
        let max_uses = reader.i32().map_err(dec_err)?;
        let xp = reader.i32().map_err(dec_err)?;
        let special_price_diff = reader.i32().map_err(dec_err)?;
        let price_multiplier = reader.f32().map_err(dec_err)?;
        let demand = reader.i32().map_err(dec_err)?;
        offers.push(ModelMerchantOffer {
            cost_a,
            cost_b,
            result,
            out_of_stock,
            uses,
            max_uses,
            xp,
            special_price_diff,
            price_multiplier,
            demand,
        });
    }
    let villager_level = reader.var_i32().map_err(dec_err)?;
    let villager_xp = reader.var_i32().map_err(dec_err)?;
    let show_progress = reader.bool().map_err(dec_err)?;
    let can_restock = reader.bool().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::MerchantOffersReceived {
        window_id,
        offers,
        villager_level,
        villager_xp,
        show_progress,
        can_restock,
    })])
}

/// Reads one `ItemCost`: item registry id, count, then a
/// `DataComponentExactPredicate`.
///
/// Returns `None` when the predicate is non-empty, which this adapter does not
/// model — see [`decode_merchant_offers`]'s doc. `EMPTY` (a zero count) is what
/// every vanilla trade sends.
fn read_item_cost(reader: &mut Reader<'_>) -> Result<Option<(i32, i32)>, AdapterError> {
    let item_id = reader.var_i32().map_err(dec_err)?;
    let count = reader.var_i32().map_err(dec_err)?;
    let predicate_count = reader.var_i32().map_err(dec_err)?;
    if predicate_count != 0 {
        return Ok(None);
    }
    Ok(Some((item_id, count)))
}

/// Decodes `ClientboundShowDialogPacket`'s Play-state form.
///
/// The field is `ByteBufCodecs.holder(Registries.DIALOG, …)`: a VarInt where `0`
/// means "an inline value follows" and `n > 0` means registry id `n - 1` with no
/// further bytes. **The off-by-one is the trap** — reading the raw VarInt as the
/// id would reference the wrong dialog for every entry.
///
/// The inline form is a `Dialog`, which is an NBT `Codec` union of six types with
/// nested body/input/action trees — a schema, not a `StreamCodec` — so it is
/// carried as raw network-NBT bytes for a renderer to parse.
fn decode_show_dialog(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let holder = reader.var_i32().map_err(dec_err)?;
    let (registry_id, inline) = if holder == 0 {
        (None, Some(reader.remaining_bytes().to_vec()))
    } else {
        (Some(holder - 1), None)
    };
    Ok(vec![Directive::Emit(ClientEvent::DialogShown {
        registry_id,
        inline,
    })])
}

/// `minecraft:map_decoration_type` registry paths by numeric id, from
/// `.cache/mc/26.2/generated/reports/registries.json`.
///
/// A **built-in** registry, so the ids are fixed by the jar rather than synced
/// during Configuration (`MapDecorationType.STREAM_CODEC` is
/// `ByteBufCodecs.holderRegistry`, a bare VarInt registry id). That is why a
/// table is correct here where it would be a guess for a dynamic registry — see
/// [`TRIM_MATERIAL_IDS`] for the contrast.
const MAP_DECORATION_TYPE_IDS: &[&str] = &[
    "player",
    "frame",
    "red_marker",
    "blue_marker",
    "target_x",
    "target_point",
    "player_off_map",
    "player_off_limits",
    "mansion",
    "monument",
    "banner_white",
    "banner_orange",
    "banner_magenta",
    "banner_light_blue",
    "banner_yellow",
    "banner_lime",
    "banner_pink",
    "banner_gray",
    "banner_light_gray",
    "banner_cyan",
    "banner_purple",
    "banner_blue",
    "banner_brown",
    "banner_green",
    "banner_red",
    "banner_black",
    "red_x",
    "village_desert",
    "village_plains",
    "village_savanna",
    "village_snowy",
    "village_taiga",
    "jungle_temple",
    "swamp_hut",
    "trial_chambers",
];
/// Decodes `ClientboundMapItemDataPacket` (id 51).
///
/// Wire shape, from the record's own `STREAM_CODEC`: a VarInt `MapId`, a `byte`
/// scale, a `bool` locked, `Optional<List<MapDecoration>>`, then
/// `MapPatch.STREAM_CODEC`'s optional.
///
/// Two traps in the patch codec, both from `MapItemSavedData.MapPatch.read`:
///
/// * the field order on the wire is **width, height, startX, startY** — *not*
///   the record's declaration order (`startX, startY, width, height`); and
/// * the optional has **no boolean tag**. A `width` of zero *is* the absent
///   case, so the four position bytes and the colour array are only present when
///   the first byte is non-zero. Reading a leading `bool` here consumes the width.
fn decode_map_item_data(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let map_id = reader.var_i32().map_err(dec_err)?;
    let scale = reader.i8().map_err(dec_err)?;
    let locked = reader.bool().map_err(dec_err)?;
    let decorations = if reader.bool().map_err(dec_err)? {
        let count = reader.var_i32().map_err(dec_err)?;
        let count = usize::try_from(count)
            .map_err(|_| AdapterError::Decode(format!("invalid map decoration count {count}")))?;
        let mut list = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let type_id = reader.var_i32().map_err(dec_err)?;
            let path = usize::try_from(type_id)
                .ok()
                .and_then(|index| MAP_DECORATION_TYPE_IDS.get(index))
                .ok_or_else(|| {
                    AdapterError::Decode(format!("unknown map decoration type id {type_id}"))
                })?;
            let x = reader.i8().map_err(dec_err)?;
            let y = reader.i8().map_err(dec_err)?;
            let rot = reader.i8().map_err(dec_err)?;
            let name = if reader.bool().map_err(dec_err)? {
                Some(Text::from_nbt(&read_network_nbt(&mut reader).map_err(dec_err)?))
            } else {
                None
            };
            list.push(MapDecoration {
                kind: parse_key(path, "map decoration type")?,
                x,
                y,
                // Vanilla's own record constructor masks this, so the client
                // never sees a rotation outside 0..=15.
                #[allow(clippy::cast_sign_loss)]
                rotation: (rot as u8) & 15,
                name,
            });
        }
        Some(list)
    } else {
        None
    };
    let width = reader.u8().map_err(dec_err)?;
    let color_patch = if width == 0 {
        None
    } else {
        let height = reader.u8().map_err(dec_err)?;
        let start_x = reader.u8().map_err(dec_err)?;
        let start_y = reader.u8().map_err(dec_err)?;
        let colors = reader.var_bytes(1 << 16).map_err(dec_err)?.to_vec();
        let expected = usize::from(width) * usize::from(height);
        if colors.len() != expected {
            return Err(AdapterError::Decode(format!(
                "map patch {width}x{height} carries {} colour bytes, expected {expected}",
                colors.len()
            )));
        }
        Some(MapPatch {
            start_x,
            start_y,
            width,
            height,
            colors,
        })
    };
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::MapItemData {
        map_id,
        scale,
        locked,
        decorations,
        color_patch,
    })])
}

/// Reads one `ItemStackTemplate` (`ItemStackTemplate.STREAM_CODEC`).
///
/// **Not** the same shape as an `ItemStack`: the template writes the item holder
/// *first* and the count second, where `ItemStack.OPTIONAL_STREAM_CODEC` leads
/// with the count and uses `<= 0` as the empty sentinel. A template is never
/// empty (its constructor rejects air and count 0), so there is no sentinel and
/// no `Option`.
fn read_item_stack_template(reader: &mut Reader<'_>) -> Result<ItemStack, AdapterError> {
    let item_id = reader.var_i32().map_err(dec_err)?;
    let name = item_name(item_id)
        .ok_or_else(|| AdapterError::Decode(format!("unknown item registry id {item_id}")))?;
    let count = reader.var_i32().map_err(dec_err)?;
    let count = u32::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid item count {count}")))?;
    let (components, complete) = read_component_patch(reader, name)?;
    if !complete {
        return Err(AdapterError::Decode(format!(
            "advancement icon {name} carries an unmodeled item component, so the rest of the packet is unreadable"
        )));
    }
    Ok(ItemStack {
        item: parse_key(name, "item")?,
        count,
        components,
    })
}

/// Decodes `ClientboundUpdateAdvancementsPacket` (id 130).
///
/// Wire shape, from the packet's own reader: a `bool` reset, a list of
/// `AdvancementHolder`, a collection of removed identifiers, a map of
/// identifier → `AdvancementProgress`, then a `bool` showAdvancements.
///
/// `DisplayInfo`'s field order is **the wire's, not the datapack schema's**, and
/// the two differ (a vendored `minecraft-data` 1.21.9 schema disagrees with 26.2
/// here): `serializeToNetwork` writes title, description, icon, frame ordinal,
/// then a **raw big-endian `int`** flag word (`writeInt`, not a byte), then the
/// background identifier only when bit 0 is set, then x and y as floats.
/// `announceChat` is not on the wire at all — vanilla's reader hardcodes
/// `false` — so bit 1 is `showToast` and bit 2 is `hidden` with nothing between.
fn decode_update_advancements(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let reset = reader.bool().map_err(dec_err)?;

    let added_count = read_count(&mut reader, "advancement")?;
    let mut added = Vec::with_capacity(added_count.min(4096));
    for _ in 0..added_count {
        let id = reader.string(32767).map_err(dec_err)?;
        let id = parse_key(&id, "advancement")?;
        let parent = if reader.bool().map_err(dec_err)? {
            let parent = reader.string(32767).map_err(dec_err)?;
            Some(parse_key(&parent, "advancement parent")?)
        } else {
            None
        };
        let display = if reader.bool().map_err(dec_err)? {
            let title = Text::from_nbt(&read_network_nbt(&mut reader).map_err(dec_err)?);
            let description = Text::from_nbt(&read_network_nbt(&mut reader).map_err(dec_err)?);
            let icon = read_item_stack_template(&mut reader)?;
            let ordinal = reader.var_i32().map_err(dec_err)?;
            let frame = AdvancementFrame::from_ordinal(ordinal).ok_or_else(|| {
                AdapterError::Decode(format!("unknown advancement frame ordinal {ordinal}"))
            })?;
            let flags = reader.i32().map_err(dec_err)?;
            let background = if flags & 1 != 0 {
                let texture = reader.string(32767).map_err(dec_err)?;
                Some(parse_key(&texture, "advancement background")?)
            } else {
                None
            };
            let x = reader.f32().map_err(dec_err)?;
            let y = reader.f32().map_err(dec_err)?;
            Some(AdvancementDisplay {
                title,
                description,
                icon,
                frame,
                background,
                show_toast: flags & 2 != 0,
                hidden: flags & 4 != 0,
                x,
                y,
            })
        } else {
            None
        };
        let group_count = read_count(&mut reader, "requirement group")?;
        let mut requirements = Vec::with_capacity(group_count.min(4096));
        for _ in 0..group_count {
            let names = read_count(&mut reader, "requirement")?;
            let mut group = Vec::with_capacity(names.min(4096));
            for _ in 0..names {
                group.push(reader.string(32767).map_err(dec_err)?);
            }
            requirements.push(group);
        }
        let sends_telemetry_event = reader.bool().map_err(dec_err)?;
        added.push(AdvancementEntry {
            id,
            parent,
            display,
            requirements,
            sends_telemetry_event,
        });
    }

    let removed_count = read_count(&mut reader, "removed advancement")?;
    let mut removed = Vec::with_capacity(removed_count.min(4096));
    for _ in 0..removed_count {
        let id = reader.string(32767).map_err(dec_err)?;
        removed.push(parse_key(&id, "removed advancement")?);
    }

    let progress_count = read_count(&mut reader, "advancement progress")?;
    let mut progress = Vec::with_capacity(progress_count.min(4096));
    for _ in 0..progress_count {
        let id = reader.string(32767).map_err(dec_err)?;
        let id = parse_key(&id, "advancement progress")?;
        let criteria_count = read_count(&mut reader, "criterion")?;
        let mut criteria = Vec::with_capacity(criteria_count.min(4096));
        for _ in 0..criteria_count {
            let name = reader.string(32767).map_err(dec_err)?;
            // `CriterionProgress` is a nullable `Instant`: a presence bool then,
            // if set, epoch millis as a big-endian long (`writeInstant`).
            let obtained = if reader.bool().map_err(dec_err)? {
                Some(reader.i64().map_err(dec_err)?)
            } else {
                None
            };
            criteria.push((name, obtained));
        }
        progress.push((id, criteria));
    }

    let show_advancements = reader.bool().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::AdvancementsUpdated {
        reset,
        added,
        removed,
        progress,
        show_advancements,
    })])
}

/// A VarInt collection length, rejected rather than truncated when negative.
fn read_count(reader: &mut Reader<'_>, what: &str) -> Result<usize, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    usize::try_from(count).map_err(|_| AdapterError::Decode(format!("invalid {what} count {count}")))
}

