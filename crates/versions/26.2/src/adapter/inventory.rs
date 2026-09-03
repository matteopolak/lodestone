//! Item, container and recipe-book packets: item component decode, slot
//! display, containers, merchant offers, advancements and stats. Split out
//! of the former monolithic `adapter.rs`.
use super::*;
// Not in `adapter::mod`'s own `lodestone_model` import list — added directly
// here rather than widening that shared glob for one type this file alone
// needs.
use lodestone_model::AttackRange;

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
            // Cap the reservation by the readable bytes: `len` is attacker
            // controlled and every stack costs at least one byte, so no more
            // than `remaining()` of them can actually be produced. Without
            // this a 9-byte payload declaring a huge count OOMs the client
            // before a single stack is read. The loop below still fails
            // honestly on a short read.
            let mut items = Vec::with_capacity(len.min(reader.remaining()));
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
            // `vanilla's own recipe book settings's own stream codec` composes four `TypeSettings`, each
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
        // ---- the remaining clientbound set -----------------------------
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
            let Some((result_items, _station_items)) = read_recipe_display(&mut reader)? else {
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
/// components (26.2 `vanilla's own data component patch's own stream codec`, the undelimited variant
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
/// Wire shape (26.2 `vanilla's own item stack's own optional stream codec`): a VarInt count — `<= 0`
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

/// `minecraft:trim_material` registry paths in the order a vanilla server
/// assigns holder ids: **sorted by resource id**, i.e. alphabetical for an
/// all-`minecraft` registry.
///
/// # Where the order comes from
///
/// The trim-material registry is a **dynamic** registry — it has no static
/// registration call sequence at all. Its entries are the JSON files
/// under `data/minecraft/trim_material/`, loaded by vanilla's own
/// resource-manager registry-load task, which registers them sorted by
/// resource id's own natural ordering. That ordering compares **path
/// first**, so for a registry whose entries are all `minecraft:` the id
/// order is plain alphabetical order of the file stems.
///
/// **Vanilla's datagen bootstrap routine for this registry is not that
/// order.** It runs against a datagen-only bootstrap context — it is the
/// *datagen* routine that writes those JSON files, and it runs in no
/// server. This table was previously transcribed from it, and the
/// resulting mapping was wrong for eight of the eleven materials: an
/// emerald trim (id 3) drew as `redstone`, which is the bootstrap list's
/// fourth entry. That is the shape this repo's evidence standards call an
/// authoritative source answering a *neighbouring* question.
///
/// # Why a table and not the synced registry
///
/// The ids arrive in Configuration's `registry_data` and
/// [`crate::packets::registry::ClientRegistries::entry_names`] does retain
/// them, but `read_component_patch` and every stack reader above it are free
/// functions with no connection state, so resolving live would mean
/// threading a registry reference through the whole stack-decoding tree.
/// Until that happens this table is exact for any server that does not
/// redefine the registry, and **provisional** for one that does — the same
/// posture as `server_protocol.rs`'s `BIOME_NAMES`. An id outside the table
/// decodes as the empty string rather than failing: the bytes are consumed
/// either way, which is the property that keeps the rest of the packet
/// readable.
///
/// `lodestone_assets::trim::TRIM_MATERIALS` is *not* read here — that table
/// answers "which sprite suffix does this material use", and its order is its
/// own business. `trim_material_ids_are_sorted_by_resource_path` is what
/// keeps this one honest.
const TRIM_MATERIAL_IDS: &[&str] = &[
    "amethyst",
    "copper",
    "diamond",
    "emerald",
    "gold",
    "iron",
    "lapis",
    "netherite",
    "quartz",
    "redstone",
    "resin",
];
/// `minecraft:trim_pattern` registry paths in the order a vanilla server
/// assigns holder ids — the `data/minecraft/trim_pattern/` file stems sorted
/// by resource id. See [`TRIM_MATERIAL_IDS`] for why that, and not the
/// equivalent datagen bootstrap routine's call order, is the id order, and
/// for the id-space caveat this table shares.
const TRIM_PATTERN_IDS: &[&str] = &[
    "bolt",
    "coast",
    "dune",
    "eye",
    "flow",
    "host",
    "raiser",
    "rib",
    "sentry",
    "shaper",
    "silence",
    "snout",
    "spire",
    "tide",
    "vex",
    "ward",
    "wayfinder",
    "wild",
];
/// Decodes `minecraft:trim`'s payload — vanilla's own armor-trim stream
/// codec, a `Holder<TrimMaterial>` then a `Holder<TrimPattern>`.
///
/// Each holder is vanilla's registry-holder codec: a VarInt
/// where `0` introduces an **inline** definition and any positive value references
/// the registry at `value - 1`. Both forms are handled, because both must be — the
/// inline form is what a datapack-defined trim arrives as, and consuming the wrong
/// number of bytes for it would desync the rest of the packet exactly as the
/// unmodeled-component cliff this arm exists to remove does.
///
/// The inline bodies, from the two direct (non-registry) stream codecs:
///
/// * [`TrimMaterial`] — vanilla's own asset-group shape (one UTF-8 string,
///   then a map of `ResourceKey -> string`, i.e. a VarInt count of
///   `(string, string)` pairs) then a description `Component` (network
///   NBT).
/// * [`TrimPattern`] — an identifier (string), a description `Component`,
///   then a `bool` `decal`.
///
/// **The inline material carries no registry name**, only its asset suffix, so
/// that is what is reported: for every vanilla material the suffix *is* the
/// registry path (confirmed against vanilla's own asset-group construction
/// helper), and it is also the half `lodestone_assets::trim::trim_sprite_id`
/// actually needs.
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

/// `minecraft:banner_pattern` registry paths in the order a vanilla server
/// assigns holder ids — the `data/minecraft/banner_pattern/` file stems
/// sorted by resource id.
///
/// `vanilla's own registries's own banner pattern` is dynamic exactly as
/// [`TRIM_MATERIAL_IDS`]' registry is (it appears in
/// `vanilla's own registry data loader's own synchronized registries` and in no built-in
/// `registries.json` report), so the same reasoning applies verbatim: the id
/// order is `vanilla's resource-manager registry-load task`'s
/// `.sorted(a by-key comparator())`, and `the equivalent datagen bootstrap routine`'s
/// register-call order — which this table previously transcribed — is a
/// datagen routine that runs in no server. Only entry `0` (`base`) happened
/// to coincide; the other 42 were shifted.
///
/// Same id-space caveat, same reason: exact for a server that does not
/// redefine the registry, provisional for one that does.
const BANNER_PATTERN_IDS: &[&str] = &[
    "base",
    "border",
    "bricks",
    "circle",
    "creeper",
    "cross",
    "curly_border",
    "diagonal_left",
    "diagonal_right",
    "diagonal_up_left",
    "diagonal_up_right",
    "flow",
    "flower",
    "globe",
    "gradient",
    "gradient_up",
    "guster",
    "half_horizontal",
    "half_horizontal_bottom",
    "half_vertical",
    "half_vertical_right",
    "mojang",
    "piglin",
    "rhombus",
    "skull",
    "small_stripes",
    "square_bottom_left",
    "square_bottom_right",
    "square_top_left",
    "square_top_right",
    "straight_cross",
    "stripe_bottom",
    "stripe_center",
    "stripe_downleft",
    "stripe_downright",
    "stripe_left",
    "stripe_middle",
    "stripe_right",
    "stripe_top",
    "triangle_bottom",
    "triangle_top",
    "triangles_bottom",
    "triangles_top",
];

/// Vanilla's own `DyeColor` stream codec (a plain id-mapper) id order —
/// its enum declaration order, `0..=15`, `WHITE` first. A bare
/// VarInt with no `+1` and no `0` sentinel, unlike the registry-holder
/// shape [`BANNER_PATTERN_IDS`] resolves — the same id-mapper-vs-holder
/// distinction [`read_pot_decorations`]' own doc documents for vanilla's
/// registry codec.
const DYE_COLOR_NAMES: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "light_gray",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

/// Decodes `minecraft:banner_patterns`' payload — vanilla's own
/// banner-pattern-layers stream codec, a list codec over each layer's own
/// stream codec: a VarInt element count (unbounded on the wire — vanilla's
/// no-arg list-codec overload caps at `the maximum i32 value`, not
/// a real limit) followed by that many layers. Each layer is a
/// `Holder<BannerPattern>` — the same registry-holder codec shape
/// [`read_armor_trim`] decodes: `0` introduces an inline `(identifier assetId,
/// String translationKey)` pair, any `n > 0` references [`BANNER_PATTERN_IDS`]
/// at `n - 1` — followed by a bare-VarInt `DyeColor`
/// (vanilla's own id-mapper codec, resolved against [`DYE_COLOR_NAMES`]).
///
/// Decoded rather than left unmodeled for the same reason as `minecraft:trim`,
/// map id, pot decorations, profile, the two book contents and bundle
/// contents above: none of `Layer`'s sub-codecs is length-prefixed, so a
/// banner or shield sitting in *any* container — inventory, chest, shulker
/// box, a loom's own input slot — used to truncate the rest of the packet
/// from that slot onward.
///
/// A layer whose pattern or colour does not resolve is **dropped**, not
/// defaulted — mirrors `lodestone_shell::block_entities`'s own
/// `banner_patterns` NBT reader (a placed banner's block-entity form of this
/// same data): a wrong-coloured or wrong-patterned layer is harder to notice
/// than a missing one. The bytes are consumed either way, which is what keeps
/// the rest of the packet aligned regardless.
///
/// Bounded at 64 layers defensively, the same margin [`read_bundle_contents`]
/// uses: vanilla's own renderer caps at [`lodestone_render`]'s
/// `MAX_PATTERN_LAYERS` (16) plus the base layer, and a survival loom cannot
/// add more than one layer per application, so a declared count above this is
/// a malformed packet, not a legitimately decorated banner or shield.
fn read_banner_pattern_layers(reader: &mut Reader<'_>) -> Result<Vec<BannerPatternLayer>, AdapterError> {
    let count = read_count(reader, "banner_patterns layer")?;
    if count > 64 {
        return Err(AdapterError::Decode(format!(
            "banner_patterns declares {count} layers, implausibly many"
        )));
    }
    let mut layers = Vec::with_capacity(count);
    for _ in 0..count {
        let pattern_asset_id = match reader.var_i32().map_err(dec_err)? {
            0 => {
                let asset_id = reader.string(32767).map_err(dec_err)?;
                let _translation_key = reader.string(32767).map_err(dec_err)?;
                Some(
                    asset_id
                        .strip_prefix("minecraft:")
                        .unwrap_or(&asset_id)
                        .to_owned(),
                )
            }
            holder => BANNER_PATTERN_IDS
                .get((holder - 1) as usize)
                .map(|s| (*s).to_owned()),
        };
        let color_id = reader.var_i32().map_err(dec_err)?;
        let color = usize::try_from(color_id)
            .ok()
            .and_then(|i| DYE_COLOR_NAMES.get(i))
            .map(|s| (*s).to_owned());
        if let (Some(pattern_asset_id), Some(color)) = (pattern_asset_id, color) {
            layers.push(BannerPatternLayer { pattern_asset_id, color });
        }
    }
    Ok(layers)
}

/// Decodes `minecraft:pot_decorations`' payload — `vanilla's own pot decorations's own stream codec`,
/// which is `vanilla's registry codec(vanilla's own registries's own item).apply(vanilla's list codec (max 4))`.
///
/// So the wire is a VarInt element count (vanilla's read-count helper, capped at 4)
/// followed by that many **bare** item registry ids as VarInts. Two shapes it is
/// easy to get wrong, both re-read from the jar rather than inferred:
///
/// * vanilla's registry codec is `idMapper`, which writes `a plain VarInt write(id)` with
///   **no `+1` and no `0` sentinel** — unlike vanilla's registry-holder codec, which
///   `minecraft:trim` uses two arms above. Adding an offset here would consume the
///   right number of bytes and report the wrong four sherds.
/// * The list is `list(4)`, a *maximum*, not a fixed width. A vanilla server
///   always writes four (`PotDecorations::ordered` builds a four-element list
///   unconditionally), but a shorter list is legal on the wire and its missing
///   tail is `an empty optional()` — `PotDecorations::getItem`'s `i >= sherds.size()`
///   arm.
///
/// `minecraft:brick` decodes to `None`, mirroring `getItem`'s
/// `item == vanilla's own items's own brick ? an empty optional() : a present optional(item)`. An id outside the
/// item registry decodes as `None` rather than failing, for the same reason
/// [`TRIM_MATERIAL_IDS`] tolerates an unknown holder: the bytes are consumed
/// either way, and that is the property keeping the rest of the packet readable.
fn read_pot_decorations(reader: &mut Reader<'_>) -> Result<PotDecorations, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    if !(0..=4).contains(&count) {
        return Err(AdapterError::Decode(format!(
            "pot_decorations declares {count} sherds; vanilla's list codec (max 4) permits 0..=4"
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

/// Decodes `minecraft:potion_contents`' payload — `vanilla's own potion contents's own stream codec`:
/// `Optional<Holder<Potion>>`, `Optional<Integer>`, `List<MobEffectInstance>`, then
/// `Optional<String>` — and folds it straight into the mixed ARGB colour via
/// [`lodestone_data::potion::potion_color`], since nothing else in this client reads
/// the raw potion id or effect list back out.
fn read_potion_contents_color(reader: &mut Reader<'_>) -> Result<u32, AdapterError> {
    // `vanilla's own potion's own stream codec = vanilla's holder-registry codec(vanilla's own registries's own potion)`: a
    // plain 0-based VarInt registry id (the same shape `minecraft:mob_effect` uses),
    // wrapped in vanilla's optional-value codec — a bool presence flag then the value.
    let potion = if reader.bool().map_err(dec_err)? {
        Some(reader.var_i32().map_err(dec_err)?)
    } else {
        None
    };
    // `vanilla's fixed-width `INT` codec.apply(vanilla's optional-value codec)`: fixed-width, not a VarInt —
    // the same `minecraft:dyed_color` trap documented above.
    let custom_color = if reader.bool().map_err(dec_err)? {
        Some(reader.i32().map_err(dec_err)? as u32)
    } else {
        None
    };
    let custom_effects = read_mob_effect_instances(reader)?;
    // `customName`: `vanilla's UTF-8 string codec.apply(vanilla's optional-value codec)`,
    // consumed for alignment only — nothing here reads it back.
    if reader.bool().map_err(dec_err)? {
        reader.string(32767).map_err(dec_err)?;
    }
    Ok(lodestone_data::potion::potion_color(potion, custom_color, &custom_effects))
}

/// Vanilla's own resolvable-profile stream codec: a composite of a
/// bool-tagged identity (full game profile or a partial one) then an
/// always-present skin-patch tail. **Both halves are read on every code
/// path**; the bool only selects *which* identity shape follows, not
/// whether the skin patch is present.
///
/// The skin patch's four fields (`body`/`cape`/`elytra` resource-id textures and
/// an optional model) are consumed for alignment and discarded: nothing in this
/// client resolves a resource-id skin override yet, and
/// [`lodestone_model::ItemProfile`] carries no field for them. Getting a width
/// wrong here would misalign every byte after this component, exactly as for the
/// "consumed-for-alignment" group below — this component just happens to be
/// *identity-bearing* for its first half and pure-alignment for its second.
fn read_resolvable_profile(reader: &mut Reader<'_>) -> Result<ItemProfile, AdapterError> {
    let profile = if reader.bool().map_err(dec_err)? {
        // vanilla's game-profile codec: uuid, then `PLAYER_NAME` (cap 16), then
        // `GAME_PROFILE_PROPERTIES` — both always present, unlike the partial
        // form below.
        let id = reader.uuid().map_err(dec_err)?;
        let name = reader.string(16).map_err(dec_err)?;
        let properties = read_profile_properties(reader)?;
        ItemProfile {
            name: Some(name),
            id: Some(id),
            properties,
        }
    } else {
        // `vanilla's own resolvable profile's own partial's own stream codec`: an optional name
        // (`PLAYER_NAME.apply(optional)`, cap 16), an optional uuid
        // (`vanilla's own uuid util's own stream codec's own apply(optional)`), then the same
        // `GAME_PROFILE_PROPERTIES` as the full form — **not** optional itself,
        // just possibly empty.
        let name = if reader.bool().map_err(dec_err)? {
            Some(reader.string(16).map_err(dec_err)?)
        } else {
            None
        };
        let id = if reader.bool().map_err(dec_err)? {
            Some(reader.uuid().map_err(dec_err)?)
        } else {
            None
        };
        let properties = read_profile_properties(reader)?;
        ItemProfile { name, id, properties }
    };

    // `vanilla's own player skin's own patch's own stream codec`: three optional `Identifier` textures
    // (`vanilla's own client asset's own resource texture's own stream codec's own apply(optional)`, each a bare
    // vanilla's UTF-8 string codec, cap 32767) then an optional `PlayerModelType`
    // (`vanilla's own player model type's own stream codec's own apply(optional)`). **The model field is a
    // bool wrapping a bool** — one presence flag, and if true, one more
    // slim/wide flag — not a single flag the way every other optional in this
    // function is; collapsing the two would misread the byte after this
    // component as the start of the next one.
    for _ in 0..3 {
        if reader.bool().map_err(dec_err)? {
            reader.string(32767).map_err(dec_err)?;
        }
    }
    if reader.bool().map_err(dec_err)? {
        reader.bool().map_err(dec_err)?; // slim/wide, discarded with the rest
    }

    Ok(profile)
}

/// `vanilla's game-profile codec_PROPERTIES`: a VarInt element count capped at 16
/// (`vanilla's read-count helper(input, 16)`), then that many `(name, value,
/// signature)` triples — name capped at 64 bytes, value at 32767, and an
/// optional signature capped at 1024. This is the exact shape
/// `player_info.rs`'s `read_add_player` already reads for `ADD_PLAYER`'s
/// property list (same codec, different packet), reimplemented here rather than
/// shared because that function lives in a sibling module this crate does not
/// expose a helper from.
fn read_profile_properties(
    reader: &mut Reader<'_>,
) -> Result<Vec<ModelProfileProperty>, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    if !(0..=16).contains(&count) {
        return Err(AdapterError::Decode(format!(
            "profile properties count {count} exceeds vanilla's read-count helper's max of 16"
        )));
    }
    let mut properties = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = reader.string(64).map_err(dec_err)?;
        let value = reader.string(32767).map_err(dec_err)?;
        let signature = if reader.bool().map_err(dec_err)? {
            Some(reader.string(1024).map_err(dec_err)?)
        } else {
            None
        };
        properties.push(ModelProfileProperty {
            name,
            value,
            signature,
        });
    }
    Ok(properties)
}

/// One `Filterable<String>` (vanilla's filterable stream codec): the raw string
/// (capped at `max`), then an optional filtered alternate (same cap),
/// discarded — see [`ItemComponents::writable_book_content`]'s own doc for
/// why only the raw half is ever worth keeping here.
fn read_filterable_string(reader: &mut Reader<'_>, max: usize) -> Result<String, AdapterError> {
    let raw = reader.string(max).map_err(dec_err)?;
    if reader.bool().map_err(dec_err)? {
        reader.string(max).map_err(dec_err)?;
    }
    Ok(raw)
}

/// Vanilla's own writable-book-content stream codec (a filterable-string
/// codec applied through a capped list codec):
/// a VarInt page count capped at 100, then that many
/// [`read_filterable_string`] entries capped at 1024 characters each.
fn read_writable_book_content(reader: &mut Reader<'_>) -> Result<Vec<String>, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    if !(0..=100).contains(&count) {
        return Err(AdapterError::Decode(format!(
            "writable_book_content page count {count} exceeds vanilla's list-codec max of 100"
        )));
    }
    let mut pages = Vec::with_capacity(count as usize);
    for _ in 0..count {
        pages.push(read_filterable_string(reader, 1024)?);
    }
    Ok(pages)
}

/// Vanilla's own written-book-content stream codec: `Filterable<String>`
/// title (cap 32), plain `author` (a UTF-8 string codec, cap 32767), VarInt
/// `generation`, a list-codec-capped (no explicit bound, so
/// `the maximum i32 value` — read defensively bounded below) list of
/// `Filterable<Component>` pages, then a `resolved` bool — in that
/// declaration order, which is also the composite stream codec's order
/// (confirmed against the decompiled 26.2 source).
///
/// Each page is read as network-NBT (vanilla's own component-serialization
/// stream codec, the same chat-component wire shape `minecraft:custom_name` and
/// `minecraft:item_name` already use) via [`Text::from_nbt`], then the
/// optional filtered alternate — network-NBT too — is read and discarded for
/// the same reason [`read_filterable_string`]'s is.
fn read_written_book_content(reader: &mut Reader<'_>) -> Result<WrittenBookContent, AdapterError> {
    let title = read_filterable_string(reader, 32)?;
    let author = reader.string(32767).map_err(dec_err)?;
    let generation = reader.var_i32().map_err(dec_err)?;
    let generation = u8::try_from(generation.clamp(0, 3)).unwrap_or(0);
    // No wire-declared cap on this list; bounded here at 4096 (vanilla's own
    // `MAX_PAGES`-adjacent constants are all far smaller) purely to keep a
    // malformed count from requesting an enormous allocation before the first
    // page is even read — the same defensive-capacity convention
    // `decode_container_click`'s own doc comment already documents.
    let count = read_count(reader, "written_book_content page")?;
    if count > 4096 {
        return Err(AdapterError::Decode(format!(
            "written_book_content declares {count} pages, implausibly many"
        )));
    }
    let mut pages = Vec::with_capacity(count.min(256));
    for _ in 0..count {
        let raw = Text::from_nbt(&read_network_nbt(reader).map_err(dec_err)?);
        if reader.bool().map_err(dec_err)? {
            read_network_nbt(reader).map_err(dec_err)?; // filtered alternate, discarded
        }
        pages.push(raw);
    }
    let resolved = reader.bool().map_err(dec_err)?;
    Ok(WrittenBookContent {
        title,
        author,
        generation,
        pages,
        resolved,
    })
}

/// `vanilla's own mob effect instance's own stream codec's own apply(vanilla's list codec())`: a VarInt count then
/// that many `(MobEffect id, vanilla's own mob effect instance's own details)` pairs. Only the effect id
/// and amplifier are kept — the colour mix needs nothing else — but every field is
/// still read off the wire in declaration order, because skipping one would
/// misalign every byte after it.
fn read_mob_effect_instances(reader: &mut Reader<'_>) -> Result<Vec<(i32, u8)>, AdapterError> {
    let count = read_count(reader, "potion custom_effects")?;
    let mut out = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        // `vanilla's own mob effect's own stream codec = vanilla's holder-registry codec(vanilla's own registries's own mob effect)`:
        // the same plain 0-based VarInt shape as the potion holder above.
        let effect_id = reader.var_i32().map_err(dec_err)?;
        let amplifier = read_mob_effect_details(reader)?;
        out.push((effect_id, amplifier));
    }
    Ok(out)
}

/// `vanilla's own mob effect instance's own details's own stream codec`: VarInt amplifier, VarInt duration, bool
/// ambient, bool showParticles, bool showIcon, then `Optional<Details>` recursing
/// into this same shape — **without** its own leading effect id, since `hiddenEffect`
/// is a nested `Details`, not a nested `MobEffectInstance`. Returns just the
/// amplifier, clamped into `u8` the way `MobEffectInstance`'s own constructor
/// (`vanilla's own mth's own clamp(amplifier, 0, 255)`) does.
fn read_mob_effect_details(reader: &mut Reader<'_>) -> Result<u8, AdapterError> {
    let amplifier = reader.var_i32().map_err(dec_err)?;
    reader.var_i32().map_err(dec_err)?; // duration
    reader.bool().map_err(dec_err)?; // ambient
    reader.bool().map_err(dec_err)?; // showParticles
    reader.bool().map_err(dec_err)?; // showIcon
    if reader.bool().map_err(dec_err)? {
        read_mob_effect_details(reader)?; // hiddenEffect, discarded
    }
    Ok(amplifier.clamp(0, 255) as u8)
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
            // Vanilla's own dyed-item-color stream codec is a bare `INT`
            // — fixed-width, not a `VarInt` like
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
            // Vanilla's own map-id stream codec is a `VAR_INT` mapped through its
            // id constructor, registered as network-synchronized in vanilla's
            // data-components table. Decoded for the same reason as the trim
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
            // Decoded for the same reason as the trim, map id and pot decorations
            // above: `minecraft:potion_contents`' payload is not length-prefixed, so
            // leaving it unmodeled truncated the rest of the packet from any potion,
            // splash potion, lingering potion or tipped arrow onward. Folded straight
            // into the mixed colour rather than kept as raw fields, since nothing
            // else in this client reads the potion id or effect list. See
            // [`read_potion_contents_color`].
            Some("minecraft:potion_contents") => {
                components.potion_color = Some(read_potion_contents_color(reader)?);
            }
            // Decoded for the same reason as the trim, map id, pot decorations and
            // potion contents above: `minecraft:profile`'s payload is not
            // length-prefixed, so a player head sitting in *any* container —
            // inventory, chest, shulker box — truncated the rest of the packet
            // from that slot onward. See [`read_resolvable_profile`].
            Some("minecraft:profile") => {
                components.profile = Some(read_resolvable_profile(reader)?);
            }
            // Decoded for the same reason as the trim, map id, pot decorations,
            // potion contents and profile above: vanilla's own
            // writable-book-content stream codec has no length prefix, so an
            // unsigned book-and-quill in any inventory used to truncate the
            // rest of the packet. See [`read_writable_book_content`].
            Some("minecraft:writable_book_content") => {
                components.writable_book_content = Some(read_writable_book_content(reader)?);
            }
            // Same reasoning, one component over: vanilla's own
            // written-book-content stream codec is equally unprefixed. See
            // [`read_written_book_content`].
            Some("minecraft:written_book_content") => {
                components.written_book_content = Some(read_written_book_content(reader)?);
            }
            // Both of these are vanilla's `VAR_INT` codec (per its data-components
            // table)
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
            // `vanilla's own repairable's own stream codec` is one `HolderSet<Item>`. The repair
            // items themselves have no consumer here, but this component is
            // unframed like every other patch payload, so consuming it is what
            // keeps a repaired item from ending the rest of its packet.
            Some("minecraft:repairable") => {
                let _repair_items = read_holder_set(reader)?;
            }
            // `vanilla's own equippable's own stream codec` is an eleven-field record. Only its
            // slot reaches `ItemComponents` today, but every field must be read:
            // a patched horse armour otherwise drops the remainder of the
            // container packet at this component.
            Some("minecraft:equippable") => {
                components.equippable = Some(read_equippable(reader)?);
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
            // a persistent-only codec and **no** network-synchronized one, so
            // vanilla's own data-component-type builder falls back to its
            // registry-aware codec-to-stream-codec bridge — which writes the
            // value as a single raw-NBT tag (root tag id then
            // payload, no name, no length prefix). One rule covers all seven, and
            // it is *not* the same codec as vanilla's own custom-data stream
            // codec, which is deprecated and used by `bucket_entity_data` rather
            // than by
            // `custom_data`. Reading either as a bare compound would be wrong for
            // `recipes` (a list tag) and for the `Unit`-valued one (an empty
            // compound from vanilla's own unit map-codec).
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

            // Vanilla's own `Unit` stream codec is a unit codec: **zero bytes**.
            // The component's presence in the patch is the whole value.
            Some(
                "minecraft:unbreakable" | "minecraft:creative_slot_lock" | "minecraft:glider",
            ) => {}

            // A bare VarInt. `rarity`, `dye` and `map_post_processing` use
            // vanilla's own id-mapper codec, which is a plain VarInt read with
            // no `+1` and no `0` sentinel; the rest are vanilla's `VAR_INT`
            // codec directly, or a one-field composite stream codec over it
            // (`enchantable`, `ominous_bottle_amplifier`).
            Some(
                "minecraft:rarity"
                | "minecraft:repair_cost"
                | "minecraft:additional_trade_cost"
                | "minecraft:ominous_bottle_amplifier"
                | "minecraft:enchantable"
                | "minecraft:dye"
                | "minecraft:map_post_processing",
            ) => {
                reader.var_i32().map_err(dec_err)?;
            }

            // `minecraft:base_color` — the same id-mapper-codec VarInt
            // shape as `dye`/`rarity` above, resolved against [`DYE_COLOR_NAMES`]
            // exactly like a `banner_patterns` layer's own colour. A shield's dye
            // tint, independent of any loom pattern layers
            // (vanilla's own shield-rendering `baseColor` field) — was previously
            // discarded with the bare-VarInt group above, which is why a shield
            // combined with a banner drew no base tint at all even once its
            // pattern layers decoded. An id outside `DYE_COLOR_NAMES` stores
            // `None`, mirroring `read_banner_pattern_layers`' own "drop, don't
            // default" rule for an unresolved colour.
            Some("minecraft:base_color") => {
                let id = reader.var_i32().map_err(dec_err)?;
                components.base_color = usize::try_from(id)
                    .ok()
                    .and_then(|i| DYE_COLOR_NAMES.get(i))
                    .map(|s| (*s).to_owned());
            }

            // Fixed-width scalars, **not** VarInts. `vanilla's own map item color's own stream codec` is
            // vanilla's fixed-width `INT` codec (the same trap `minecraft:dyed_color` documents
            // above), and the two floats are vanilla's fixed-width `FLOAT` codec.
            Some("minecraft:map_color") => {
                reader.i32().map_err(dec_err)?;
            }
            Some("minecraft:minimum_attack_charge" | "minecraft:potion_duration_scale") => {
                reader.f32().map_err(dec_err)?;
            }
            Some("minecraft:enchantment_glint_override") => {
                reader.bool().map_err(dec_err)?;
            }

            // vanilla's identifier stream codec is `vanilla's UTF-8 string codec.map(...)`:
            // one length-prefixed string, capped at 32767.
            Some("minecraft:item_model") => {
                let raw = reader.string(32767).map_err(dec_err)?;
                components.item_model = Some(raw.parse().map_err(|error| {
                    AdapterError::Decode(format!("invalid minecraft:item_model identifier {raw:?}: {error}"))
                })?);
            }
            Some("minecraft:tooltip_style" | "minecraft:note_block_sound") => {
                reader.string(32767).map_err(dec_err)?;
            }

            // `vanilla's own component serialization's own stream codec` — the same network-NBT chat
            // component `minecraft:custom_name` uses. `item_name` is the *item's*
            // name rather than a rename, so it is consumed and not surfaced;
            // nothing here prefers it over `custom_name`.
            Some("minecraft:item_name") => {
                read_network_nbt(reader).map_err(dec_err)?;
            }

            // `vanilla's own item lore's own stream codec` is `vanilla's own component serialization's own stream codec
            // .apply(vanilla's list codec (max 256))`: a VarInt count then that many
            // network-NBT components. 256 is the codec's own cap.
            Some("minecraft:lore") => {
                let lines = read_count(reader, "lore line")?;
                if lines > 256 {
                    return Err(AdapterError::Decode(format!(
                        "lore declares {lines} lines; vanilla's list codec (max 256) permits at most 256"
                    )));
                }
                components.lore.reserve(lines);
                for _ in 0..lines {
                    let line = read_network_nbt(reader).map_err(dec_err)?;
                    components.lore.push(Text::from_nbt(&line));
                }
            }

            // `stored_enchantments` shares `vanilla's own item enchantments's own stream codec` with
            // `minecraft:enchantments`, so it reuses that reader — but it is an
            // enchanted *book*'s payload, not the stack's own effects, so it is
            // deliberately not merged into `components.enchantments`.
            Some("minecraft:stored_enchantments") => {
                read_enchantments(reader)?;
            }

            Some("minecraft:custom_model_data") => {
                components.custom_model_data = read_custom_model_data(reader)?;
            }
            Some("minecraft:tooltip_display") => read_tooltip_display(reader)?,
            Some("minecraft:attribute_modifiers") => read_attribute_modifiers(reader)?,

            // Decoded for the same reason as the trim, map id, pot decorations,
            // profile and the two book contents above: `ItemStackTemplate
            // .STREAM_CODEC` (`BundleContents`' per-entry codec) has no length
            // prefix, so a filled bundle sitting in any inventory truncated the
            // rest of the packet from that slot onward. See
            // [`read_bundle_contents`].
            Some("minecraft:bundle_contents") => {
                let (items, complete) = read_bundle_contents(reader)?;
                components.bundle_contents = items;
                if !complete {
                    components.has_unmodeled = true;
                    let _ = reader.bytes(reader.remaining());
                    return Ok((components, false));
                }
            }

            // Decoded for the same reason as the trim, map id, pot decorations,
            // profile, the two book contents and bundle contents above: none of
            // `vanilla's own banner pattern layers's own layer`'s sub-codecs is length-prefixed, so a
            // banner or shield in any container truncated the rest of the packet
            // from that slot onward. See [`read_banner_pattern_layers`].
            Some("minecraft:banner_patterns") => {
                components.banner_patterns = read_banner_pattern_layers(reader)?;
            }

            // Decoded for the same reason as bundle_contents immediately above:
            // each entry shares that same item-then-count-then-recursive-patch
            // per-entry shape and carries no length prefix, so a loaded
            // crossbow sitting in any container truncated the rest of the
            // packet from that slot onward. See [`read_charged_projectiles`]
            // and `docs/item-data-component-decode.md` for the wire citation.
            Some("minecraft:charged_projectiles") => {
                let (items, complete) = read_charged_projectiles(reader)?;
                components.charged_projectiles = items;
                if !complete {
                    components.has_unmodeled = true;
                    let _ = reader.bytes(reader.remaining());
                    return Ok((components, false));
                }
            }

            // Six fixed-width floats (`f32`, not VarInts), no length prefix —
            // the same class of decode cliff as trim, map id and the rest of
            // that group: a spear-family item in any container truncated the
            // rest of the packet from that slot onward. Wire order is
            // min reach, max reach, min creative reach, max creative reach,
            // hitbox margin, mob factor — see `docs/item-data-component-decode.md`
            // for the wire citation.
            Some("minecraft:attack_range") => {
                let min_reach = reader.f32().map_err(dec_err)?;
                let max_reach = reader.f32().map_err(dec_err)?;
                let min_creative_reach = reader.f32().map_err(dec_err)?;
                let max_creative_reach = reader.f32().map_err(dec_err)?;
                let hitbox_margin = reader.f32().map_err(dec_err)?;
                let mob_factor = reader.f32().map_err(dec_err)?;
                components.attack_range = Some(AttackRange::new(
                    min_reach,
                    max_reach,
                    min_creative_reach,
                    max_creative_reach,
                    hitbox_margin,
                    mob_factor,
                ));
            }

            // `vanilla's own use effects's own stream codec`: two bools (canSprint, interactVibrations)
            // then a float (speedMultiplier) — an eating/drinking speed-and-motion
            // modifier with no current consumer here; unframed like the rest of
            // this group, so a consumable stack carrying it would otherwise
            // truncate the packet from that slot onward.
            Some("minecraft:use_effects") => {
                reader.bool().map_err(dec_err)?;
                reader.bool().map_err(dec_err)?;
                reader.f32().map_err(dec_err)?;
            }

            // `vanilla's own adventure mode predicate's own stream codec` is a `List<BlockPredicate>`, and
            // `BlockPredicate`'s own codec carries a `DataComponentMatchers`, whose
            // `partial` half dispatches through a *second*, independent registry
            // (`data_component_predicate_type`, 15 entries) — several of which
            // (`container`, `bundle_contents`) embed an item/collection predicate
            // that recurses back into another `DataComponentMatchers`. Walking that
            // byte-accurately is not "one more component reader"; it is a second,
            // general-purpose predicate interpreter with no length prefix anywhere
            // in the chain to fall back on if one of its own 15 sub-types is itself
            // unrecognised. Genuinely unskippable without building that interpreter,
            // the same way the `explode` packet's non-simple particle ids are —
            // every other component in this match *is* modeled; these two are the
            // one deliberate exception.
            Some(name @ ("minecraft:can_place_on" | "minecraft:can_break")) => {
                components.has_unmodeled = true;
                tracing::warn!(
                    item,
                    component = name,
                    component_id = type_id,
                    "unmodeled item data component (predicate recursion has no length \
                     prefix to fall back on); delivering a partial stack and skipping \
                     the rest of the packet",
                );
                let _ = reader.bytes(reader.remaining());
                return Ok((components, false));
            }

            // `vanilla's own food properties's own direct stream codec`: VarInt nutrition, float
            // saturation, bool canAlwaysEat.
            Some("minecraft:food") => {
                reader.var_i32().map_err(dec_err)?;
                reader.f32().map_err(dec_err)?;
                reader.bool().map_err(dec_err)?;
            }

            // `vanilla's own consumable's own stream codec`: float consumeSeconds, `ItemUseAnimation`
            // (a bare `idMapper` VarInt), a `Holder<SoundEvent>`, bool
            // hasConsumeParticles, then the same `List<ConsumeEffect>` shape
            // `minecraft:death_protection` carries — see [`read_consume_effects`].
            Some("minecraft:consumable") => {
                reader.f32().map_err(dec_err)?;
                reader.var_i32().map_err(dec_err)?; // ItemUseAnimation
                read_sound_event_holder(reader)?;
                reader.bool().map_err(dec_err)?;
                if !read_consume_effects(reader)? {
                    components.has_unmodeled = true;
                    let _ = reader.bytes(reader.remaining());
                    return Ok((components, false));
                }
            }

            // `vanilla's own use remainder's own stream codec` is a single `ItemStackTemplate` — the
            // stack an eaten/drunk item converts into (an empty bottle, a bowl).
            // Unframed like the rest of this group.
            Some("minecraft:use_remainder") => {
                let (_, complete) = read_item_stack_template_tolerant(reader)?;
                if !complete {
                    components.has_unmodeled = true;
                    let _ = reader.bytes(reader.remaining());
                    return Ok((components, false));
                }
            }

            // `vanilla's own use cooldown's own stream codec`: float seconds, then an optional
            // `Identifier` cooldown-group override (bool then a bare UTF8 string).
            Some("minecraft:use_cooldown") => {
                reader.f32().map_err(dec_err)?;
                if reader.bool().map_err(dec_err)? {
                    reader.string(32767).map_err(dec_err)?;
                }
            }

            // `vanilla's own damage resistant's own stream codec` is a single, non-optional
            // `HolderSet<DamageType>` — the same wire shape [`read_holder_set`]
            // already reads for `Repairable`'s item set.
            Some("minecraft:damage_resistant") => {
                let _ = read_holder_set(reader)?;
            }

            // `vanilla's own weapon's own stream codec`: VarInt itemDamagePerAttack, float
            // disableBlockingForSeconds.
            Some("minecraft:weapon") => {
                reader.var_i32().map_err(dec_err)?;
                reader.f32().map_err(dec_err)?;
            }

            // `vanilla's own death protection's own stream codec` is a single `List<ConsumeEffect>` — a
            // totem-of-undying-shaped item's on-death effect list. See
            // [`read_consume_effects`]; an unrecognised `ConsumeEffect` variant is
            // itself an unframed dispatch this decoder cannot see past, so the same
            // truncation applies as for `minecraft:consumable` above.
            Some("minecraft:death_protection") => {
                if !read_consume_effects(reader)? {
                    components.has_unmodeled = true;
                    let _ = reader.bytes(reader.remaining());
                    return Ok((components, false));
                }
            }

            // Vanilla's own blocks-attacks stream codec: float blockDelaySeconds,
            // float disableCooldownScale, `List<DamageReduction>` (float
            // horizontalBlockingAngle, `Optional<HolderSet<DamageType>>`, float
            // base, float factor — four fields, in that order), one
            // item-damage-function record (float threshold, float base, float
            // factor — not a list), `Optional<HolderSet<DamageType>>` bypassedBy,
            // then two `Optional<Holder<SoundEvent>>` (blockSound, disableSound).
            Some("minecraft:blocks_attacks") => {
                reader.f32().map_err(dec_err)?;
                reader.f32().map_err(dec_err)?;
                let reductions = read_count(reader, "blocks_attacks damage_reductions")?;
                if reductions > 256 {
                    return Err(AdapterError::Decode(format!(
                        "blocks_attacks declares {reductions} damage reductions, implausibly many"
                    )));
                }
                for _ in 0..reductions {
                    reader.f32().map_err(dec_err)?; // horizontalBlockingAngle
                    if reader.bool().map_err(dec_err)? {
                        let _ = read_holder_set(reader)?; // type
                    }
                    reader.f32().map_err(dec_err)?; // base
                    reader.f32().map_err(dec_err)?; // factor
                }
                reader.f32().map_err(dec_err)?; // item-damage-function threshold
                reader.f32().map_err(dec_err)?; // item-damage-function base
                reader.f32().map_err(dec_err)?; // item-damage-function factor
                if reader.bool().map_err(dec_err)? {
                    let _ = read_holder_set(reader)?; // bypassedBy
                }
                if reader.bool().map_err(dec_err)? {
                    read_sound_event_holder(reader)?; // blockSound
                }
                if reader.bool().map_err(dec_err)? {
                    read_sound_event_holder(reader)?; // disableSound
                }
            }

            // `vanilla's own piercing weapon's own stream codec`: two bools (dealsKnockback, dismounts)
            // then two `Optional<Holder<SoundEvent>>` (sound, hitSound).
            Some("minecraft:piercing_weapon") => {
                reader.bool().map_err(dec_err)?;
                reader.bool().map_err(dec_err)?;
                if reader.bool().map_err(dec_err)? {
                    read_sound_event_holder(reader)?;
                }
                if reader.bool().map_err(dec_err)? {
                    read_sound_event_holder(reader)?;
                }
            }

            // `vanilla's own kinetic weapon's own stream codec`: two VarInts (contactCooldownTicks,
            // delayTicks), three `Optional<Condition>` (each a VarInt
            // maxDurationTicks then two floats — minSpeed, minRelativeSpeed), two
            // floats (forwardMovement, damageMultiplier), then two
            // `Optional<Holder<SoundEvent>>` (sound, hitSound).
            Some("minecraft:kinetic_weapon") => {
                reader.var_i32().map_err(dec_err)?;
                reader.var_i32().map_err(dec_err)?;
                for _ in 0..3 {
                    if reader.bool().map_err(dec_err)? {
                        reader.var_i32().map_err(dec_err)?;
                        reader.f32().map_err(dec_err)?;
                        reader.f32().map_err(dec_err)?;
                    }
                }
                reader.f32().map_err(dec_err)?;
                reader.f32().map_err(dec_err)?;
                if reader.bool().map_err(dec_err)? {
                    read_sound_event_holder(reader)?;
                }
                if reader.bool().map_err(dec_err)? {
                    read_sound_event_holder(reader)?;
                }
            }

            // `vanilla's own swing animation's own stream codec`: `SwingAnimationType` (a bare
            // `idMapper` VarInt) then a VarInt duration.
            Some("minecraft:swing_animation") => {
                reader.var_i32().map_err(dec_err)?;
                reader.var_i32().map_err(dec_err)?;
            }

            // `vanilla's own suspicious stew effects's own stream codec`: a list of (`MobEffect` holder —
            // the same bare `holderRegistry` VarInt `minecraft:potion_contents`'s
            // custom effects use — then a VarInt duration) pairs.
            Some("minecraft:suspicious_stew_effects") => {
                let count = read_count(reader, "suspicious_stew_effects entry")?;
                if count > 256 {
                    return Err(AdapterError::Decode(format!(
                        "suspicious_stew_effects declares {count} entries, implausibly many"
                    )));
                }
                for _ in 0..count {
                    reader.var_i32().map_err(dec_err)?; // MobEffect holder
                    reader.var_i32().map_err(dec_err)?; // duration
                }
            }

            // `vanilla's typed-entity-data stream codec(vanilla's own entity type's own stream codec)`: a bare
            // registry VarInt (`EntityType`) then a network-NBT compound tag. See
            // [`read_typed_entity_data`].
            Some("minecraft:entity_data") => {
                read_typed_entity_data(reader)?;
            }

            // `vanilla's own custom data's own stream codec` here (unlike plain `minecraft:custom_data`
            // above, which has no `networkSynchronized` at all) is
            // vanilla's compound-tag codec directly — one network-NBT compound tag,
            // no leading type id.
            Some("minecraft:bucket_entity_data") => {
                read_network_nbt(reader).map_err(dec_err)?;
            }

            // `vanilla's typed-entity-data stream codec(vanilla's registry codec(BLOCK_ENTITY_TYPE))`:
            // the same shape as `minecraft:entity_data` above, keyed by
            // `BlockEntityType` instead. Found live: a `minecraft:spawner` stack
            // truncated the rest of the packet from here on while this was
            // unmodeled.
            Some("minecraft:block_entity_data") => {
                read_typed_entity_data(reader)?;
            }

            // `vanilla's own instrument's own stream codec = vanilla's registry-holder codec(vanilla's own registries's own instrument,
            // DIRECT_STREAM_CODEC)`: `0` for an inline instrument (a
            // `Holder<SoundEvent>`, then two floats — useDuration, range — then a
            // network-NBT chat component description), a positive value for a bare
            // registry reference (`id + 1`, no body) — the same vanilla's registry-holder codec
            // discriminator [`read_sound_event_holder`] already reads.
            Some("minecraft:instrument") => {
                if reader.var_i32().map_err(dec_err)? == 0 {
                    read_sound_event_holder(reader)?;
                    reader.f32().map_err(dec_err)?;
                    reader.f32().map_err(dec_err)?;
                    read_network_nbt(reader).map_err(dec_err)?;
                }
            }

            // `vanilla's own trim material's own stream codec = vanilla's registry-holder codec(vanilla's own registries's own trim material,
            // DIRECT_STREAM_CODEC)`: same `0`-inline / `id + 1`-reference shape as
            // `minecraft:instrument` above. The inline body is a
            // `MaterialAssetGroup` (a base asset-info UTF8 string, then a
            // resource-key-to-asset-info override table, each entry two UTF8
            // strings) followed by a network-NBT chat component description.
            Some("minecraft:provides_trim_material") => {
                if reader.var_i32().map_err(dec_err)? == 0 {
                    reader.string(32767).map_err(dec_err)?; // base AssetInfo
                    let overrides = read_count(reader, "provides_trim_material override")?;
                    if overrides > 256 {
                        return Err(AdapterError::Decode(format!(
                            "provides_trim_material declares {overrides} overrides, implausibly many"
                        )));
                    }
                    for _ in 0..overrides {
                        reader.string(32767).map_err(dec_err)?; // ResourceKey
                        reader.string(32767).map_err(dec_err)?; // AssetInfo
                    }
                    read_network_nbt(reader).map_err(dec_err)?;
                }
            }

            // `vanilla's own jukebox playable's own stream codec` is a single `Holder<JukeboxSong>`;
            // `vanilla's own jukebox song's own stream codec` uses the same vanilla's registry-holder codec
            // discriminator again. The inline body is a `Holder<SoundEvent>`, a
            // network-NBT chat component description, a float lengthInSeconds and
            // a VarInt comparatorOutput.
            Some("minecraft:jukebox_playable") => {
                if reader.var_i32().map_err(dec_err)? == 0 {
                    read_sound_event_holder(reader)?;
                    read_network_nbt(reader).map_err(dec_err)?;
                    reader.f32().map_err(dec_err)?;
                    reader.var_i32().map_err(dec_err)?;
                }
            }

            // `vanilla's holder-set codec(vanilla's own registries's own banner pattern)` — the same
            // `HolderSet<T>` shape [`read_holder_set`] already reads.
            Some("minecraft:provides_banner_patterns") => {
                let _ = read_holder_set(reader)?;
            }

            // `vanilla's own lodestone tracker's own stream codec`: an `Optional<GlobalPos>` (bool, then
            // a `ResourceKey<Level>` — a bare UTF8 identifier string — and a
            // packed-`i64` `BlockPos`), then a bool `tracked`.
            Some("minecraft:lodestone_tracker") => {
                if reader.bool().map_err(dec_err)? {
                    reader.string(32767).map_err(dec_err)?; // dimension
                    reader.i64().map_err(dec_err)?; // packed BlockPos
                }
                reader.bool().map_err(dec_err)?; // tracked
            }

            // See [`read_firework_explosion`].
            Some("minecraft:firework_explosion") => {
                read_firework_explosion(reader)?;
            }

            // `vanilla's own fireworks's own stream codec`: VarInt flightDuration, then a
            // `List<FireworkExplosion>` capped at 256 — [`read_firework_explosion`]
            // per entry.
            Some("minecraft:fireworks") => {
                reader.var_i32().map_err(dec_err)?;
                let count = read_count(reader, "fireworks explosion")?;
                if count > 256 {
                    return Err(AdapterError::Decode(format!(
                        "fireworks declares {count} explosions; vanilla's list codec's max is 256"
                    )));
                }
                for _ in 0..count {
                    read_firework_explosion(reader)?;
                }
            }

            // `vanilla's own item container contents's own stream codec`: a `List<Optional<ItemStackTemplate>>`
            // capped at 256 — a shulker box's, chest boat's or bundle-adjacent
            // container's slot contents. Each present entry is
            // [`read_item_stack_template_tolerant`]; an unmodeled component inside
            // one slot is exactly as unrecoverable as at the top level.
            Some("minecraft:container") => {
                let count = read_count(reader, "container item")?;
                if count > 256 {
                    return Err(AdapterError::Decode(format!(
                        "container declares {count} items; vanilla's list codec's max is 256"
                    )));
                }
                for _ in 0..count {
                    if reader.bool().map_err(dec_err)? {
                        let (_, complete) = read_item_stack_template_tolerant(reader)?;
                        if !complete {
                            components.has_unmodeled = true;
                            let _ = reader.bytes(reader.remaining());
                            return Ok((components, false));
                        }
                    }
                }
            }

            // `vanilla's own block item state properties's own stream codec` is a bare
            // `Map<String, String>` — property name to serialised value, for a
            // block item placed with a specific state (`/give … [block_state={…}]`).
            // No wire-declared cap; bounded defensively — no vanilla block carries
            // anywhere near this many properties.
            Some("minecraft:block_state") => {
                let count = read_count(reader, "block_state property")?;
                if count > 256 {
                    return Err(AdapterError::Decode(format!(
                        "block_state declares {count} properties, implausibly many"
                    )));
                }
                for _ in 0..count {
                    reader.string(32767).map_err(dec_err)?; // property name
                    reader.string(32767).map_err(dec_err)?; // property value
                }
            }

            // `vanilla's own bees's own stream codec`: a `List<Occupant>`, each a
            // [`read_typed_entity_data`] (`EntityType`-keyed) followed by two
            // VarInts (ticksInHive, minTicksInHive). No wire-declared cap; a
            // beehive holds at most three, so bounded defensively.
            Some("minecraft:bees") => {
                let count = read_count(reader, "bees occupant")?;
                if count > 64 {
                    return Err(AdapterError::Decode(format!(
                        "bees declares {count} occupants, implausibly many"
                    )));
                }
                for _ in 0..count {
                    read_typed_entity_data(reader)?;
                    reader.var_i32().map_err(dec_err)?; // ticksInHive
                    reader.var_i32().map_err(dec_err)?; // minTicksInHive
                }
            }

            // `vanilla's own sulfur cube content's own stream codec` is a single, non-optional
            // `ItemStackTemplate` — the block item a sulfur cube has absorbed.
            Some("minecraft:sulfur_cube_content") => {
                let (_, complete) = read_item_stack_template_tolerant(reader)?;
                if !complete {
                    components.has_unmodeled = true;
                    let _ = reader.bytes(reader.remaining());
                    return Ok((components, false));
                }
            }

            // `vanilla's own sound event's own stream codec` directly (not optional) — the same
            // vanilla's registry-holder codec discriminator [`read_sound_event_holder`]
            // already reads.
            Some("minecraft:break_sound") => {
                read_sound_event_holder(reader)?;
            }

            // `vanilla's own painting variant's own stream codec = vanilla's registry-holder codec(vanilla's own registries's own painting variant,
            // DIRECT_STREAM_CODEC)`: same `0`-inline / `id + 1`-reference shape as
            // `minecraft:instrument` above. The inline body is two VarInts (width,
            // height), a bare UTF8 identifier (assetId), then two
            // `Optional<Component>` network-NBT chat components (title, author).
            Some("minecraft:painting/variant") => {
                if reader.var_i32().map_err(dec_err)? == 0 {
                    reader.var_i32().map_err(dec_err)?; // width
                    reader.var_i32().map_err(dec_err)?; // height
                    reader.string(32767).map_err(dec_err)?; // assetId
                    if reader.bool().map_err(dec_err)? {
                        read_network_nbt(reader).map_err(dec_err)?; // title
                    }
                    if reader.bool().map_err(dec_err)? {
                        read_network_nbt(reader).map_err(dec_err)?; // author
                    }
                }
            }

            // A single bare, 0-based VarInt with no framing beyond it — either
            // vanilla's holder-registry codec (a synced-registry `Holder<T>`
            // reference: `damage_type` and every `Holder<…Variant>`/
            // `Holder<…SoundVariant>` below) or vanilla's id-mapper codec (a
            // `StringRepresentable` enum ordinal: every plain, non-`Holder`
            // `…Variant`/`DyeColor` field below) — both shapes are one VarInt on
            // the wire with no discriminator, so they share this arm. Consumed for
            // alignment only, the same as `minecraft:rarity`'s group above: mostly
            // bucket-item variant fields (`tropical_fish/*`, `salmon/size`,
            // `axolotl/variant`, …) and mob variant/collar fields, none of which
            // this client renders from an item stack today.
            Some(
                "minecraft:damage_type"
                | "minecraft:villager/variant"
                | "minecraft:wolf/variant"
                | "minecraft:wolf/sound_variant"
                | "minecraft:wolf/collar"
                | "minecraft:fox/variant"
                | "minecraft:salmon/size"
                | "minecraft:parrot/variant"
                | "minecraft:tropical_fish/pattern"
                | "minecraft:tropical_fish/base_color"
                | "minecraft:tropical_fish/pattern_color"
                | "minecraft:mooshroom/variant"
                | "minecraft:rabbit/variant"
                | "minecraft:pig/variant"
                | "minecraft:pig/sound_variant"
                | "minecraft:cow/variant"
                | "minecraft:cow/sound_variant"
                | "minecraft:chicken/variant"
                | "minecraft:chicken/sound_variant"
                | "minecraft:zombie_nautilus/variant"
                | "minecraft:frog/variant"
                | "minecraft:horse/variant"
                | "minecraft:llama/variant"
                | "minecraft:axolotl/variant"
                | "minecraft:cat/variant"
                | "minecraft:cat/sound_variant"
                | "minecraft:cat/collar"
                | "minecraft:sheep/color"
                | "minecraft:shulker/color",
            ) => {
                reader.var_i32().map_err(dec_err)?;
            }

            other => {
                // An unmodeled component: its payload is not length-prefixed, so
                // it and everything after it in this packet are unreadable. Keep
                // the modeled fields decoded so far, flag the stack, and stop —
                // the packet is dropped past this point, not fatal.
                //
                // **Skipping is genuinely impossible here, re-verified against the
                // jar rather than inherited from this comment.** 26.2 has two patch
                // codecs: vanilla's own undelimited stream codec writes each
                // payload raw and its delimited variant length-prefixes it
                // (confirmed against the decompiled source). Clientbound stacks
                // use the item-stack optional stream codec, built on the
                // **undelimited** one; the delimited variant is the
                // untrusted-optional one, i.e. serverbound only. So there is no
                // length to skip and no self-describing framing to walk. The only
                // way to stop a given component being a decode cliff is to model
                // it, which is what the `minecraft:trim` arm above does.
                //
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
            // not 64 (confirmed against the decompiled item-stack source) —
            // so this is a real, if exotic, way to make an item unstackable.
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

/// Consumes `vanilla's own consume effect's own stream codec's own apply(vanilla's list codec())` — the
/// payload shape shared by `minecraft:consumable`'s `onConsumeEffects` and
/// `minecraft:death_protection`'s `deathEffects`.
///
/// Each entry leads with a bare, 0-based VarInt naming one of the five
/// `minecraft:consume_effect_type` registry entries (`apply_effects`,
/// `remove_effects`, `clear_all_effects`, `teleport_randomly`, `play_sound`,
/// in registration order) and dispatches to that type's own composite codec.
/// A sixth, future/datapack-defined type has no generic fallback — the same
/// unframed-dispatch cliff [`read_component_patch`]'s own `other` arm
/// documents — so this returns `false` rather than erroring, letting the
/// caller apply the same has-unmodeled-component treatment.
///
/// Bounded at 1024 entries defensively: the codec itself declares no cap
/// (`vanilla's list codec()` with no argument).
fn read_consume_effects(reader: &mut Reader<'_>) -> Result<bool, AdapterError> {
    let count = read_count(reader, "consume effect")?;
    if count > 1024 {
        return Err(AdapterError::Decode(format!(
            "consume effect list declares {count} entries, implausibly many"
        )));
    }
    for _ in 0..count {
        let type_id = reader.var_i32().map_err(dec_err)?;
        match type_id {
            // apply_effects: List<MobEffectInstance> (the same shape
            // [`read_mob_effect_instances`] already reads for potion custom
            // effects) then a float probability.
            0 => {
                let _ = read_mob_effect_instances(reader)?;
                reader.f32().map_err(dec_err)?;
            }
            // remove_effects: HolderSet<MobEffect>.
            1 => {
                let _ = read_holder_set(reader)?;
            }
            // clear_all_effects: no payload — presence alone is the value.
            2 => {}
            // teleport_randomly: a float diameter.
            3 => {
                reader.f32().map_err(dec_err)?;
            }
            // play_sound: Holder<SoundEvent>.
            4 => {
                read_sound_event_holder(reader)?;
            }
            _ => return Ok(false),
        }
    }
    Ok(true)
}

/// Consumes a `TypedEntityData<T>.STREAM_CODEC` (vanilla's typed-entity-data stream codec):
/// a bare, 0-based registry VarInt naming the entity/block-entity type, then a
/// network-NBT compound tag. Shared by `minecraft:entity_data`
/// (`EntityType`-keyed), `minecraft:block_entity_data`
/// (`BlockEntityType`-keyed) and each entry of `minecraft:bees`' occupant list
/// (`EntityType`-keyed) — the leading id's registry differs per caller, but
/// its wire shape (a plain vanilla's registry codec VarInt) does not.
fn read_typed_entity_data(reader: &mut Reader<'_>) -> Result<(), AdapterError> {
    reader.var_i32().map_err(dec_err)?;
    read_network_nbt(reader).map_err(dec_err)?;
    Ok(())
}

/// Reads one `ItemStackTemplate` (item id, count, then a nested, recursive
/// `DataComponentPatch`) and reports whether the nested patch decoded to
/// completion, instead of [`read_item_stack_template`]'s hard failure on an
/// unmodeled nested component.
///
/// Shared by `minecraft:use_remainder`, `minecraft:container` and
/// `minecraft:sulfur_cube_content`: an unmodeled component inside one of
/// these nested stacks is exactly as unrecoverable as one at the top level
/// (no length prefix either), so the caller applies the same
/// has-unmodeled-component treatment rather than failing the whole packet —
/// a shulker box with an unusual item in one slot is not a case this decoder
/// can afford to treat as fatal.
fn read_item_stack_template_tolerant(
    reader: &mut Reader<'_>,
) -> Result<(ItemStack, bool), AdapterError> {
    let item_id = reader.var_i32().map_err(dec_err)?;
    let name = item_name(item_id)
        .ok_or_else(|| AdapterError::Decode(format!("unknown item registry id {item_id}")))?;
    let count = reader.var_i32().map_err(dec_err)?;
    let count = u32::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid item count {count}")))?;
    let (components, complete) = read_component_patch(reader, name)?;
    Ok((
        ItemStack {
            item: parse_key(name, "item")?,
            count,
            components,
        },
        complete,
    ))
}

/// Consumes one `vanilla's own firework explosion's own stream codec`: `Shape` (a bare `idMapper`
/// VarInt), a VarInt-counted `colors` list of fixed-width `i32`s, a
/// same-shaped `fadeColors` list, then two bools (hasTrail, hasTwinkle).
/// Shared by the top-level `minecraft:firework_explosion` component and each
/// entry of `minecraft:fireworks`' explosion list.
///
/// Both colour lists are bounded at 256 entries defensively: neither codec
/// declares a cap (`vanilla's fixed-width `INT` codec.apply(vanilla's list codec())`), but no
/// legitimate firework star carries anywhere near that many colours.
fn read_firework_explosion(reader: &mut Reader<'_>) -> Result<(), AdapterError> {
    reader.var_i32().map_err(dec_err)?; // Shape
    let colors = read_count(reader, "firework_explosion color")?;
    if colors > 256 {
        return Err(AdapterError::Decode(format!(
            "firework_explosion declares {colors} colors, implausibly many"
        )));
    }
    for _ in 0..colors {
        reader.i32().map_err(dec_err)?;
    }
    let fade_colors = read_count(reader, "firework_explosion fade_color")?;
    if fade_colors > 256 {
        return Err(AdapterError::Decode(format!(
            "firework_explosion declares {fade_colors} fade colors, implausibly many"
        )));
    }
    for _ in 0..fade_colors {
        reader.i32().map_err(dec_err)?;
    }
    reader.bool().map_err(dec_err)?; // hasTrail
    reader.bool().map_err(dec_err)?; // hasTwinkle
    Ok(())
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
/// (`vanilla's own custom model data's own stream codec`).
///
/// Four independent VarInt-counted lists, in order: floats, flags (bools),
/// strings, colours. **The colours are vanilla's fixed-width `INT` codec** — fixed-width
/// big-endian, not VarInts — which is the one width in this component that a
/// VarInt-by-default reader gets wrong, and getting it wrong misaligns the whole
/// rest of the packet instead of merely losing a colour.
fn read_custom_model_data(reader: &mut Reader<'_>) -> Result<Vec<u32>, AdapterError> {
    let floats = read_count(reader, "custom_model_data float")?;
    let mut numeric = Vec::with_capacity(floats);
    for _ in 0..floats {
        numeric.push(reader.f32().map_err(dec_err)?.to_bits());
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
    Ok(numeric)
}

/// Consumes a `minecraft:tooltip_display` payload (`vanilla's own tooltip display's own stream codec`).
///
/// A bool `hideTooltip`, then a VarInt-counted collection of
/// `vanilla's own data component type's own stream codec` — which is vanilla's registry codec, i.e. a
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
/// (`vanilla's own item attribute modifiers's own stream codec`).
///
/// A VarInt-counted list of `Entry`, each of which is, in wire order:
///
/// * the attribute as `vanilla's own attribute's own stream codec` = vanilla's holder-registry codec,
///   a **bare** VarInt registry id — `holderRegistry` is `registry(…,
///   Registry::asHolderIdMap)`, so unlike vanilla's registry-holder codec there is no `+1`
///   and no inline-holder `0` sentinel;
/// * the modifier as `vanilla's own attribute modifier's own stream codec` — an `Identifier` string, a
///   **`vanilla's own byte buf codecs's own double`** (fixed-width f64, not a float), then the operation
///   as an idMapper VarInt;
/// * the slot group as `vanilla's own equipment slot group's own stream codec`, an idMapper VarInt;
/// * the display as `vanilla's own display's own stream codec`, a VarInt `vanilla's own display's own type` id dispatching
///   to a payload: `default` (0) and `hidden` (1) are vanilla's unit stream codec, i.e.
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
            // `default` and `hidden` are vanilla's unit stream codec: no payload.
            0 | 1 => {}
            // `override` carries the replacement text.
            2 => {
                read_network_nbt(reader).map_err(dec_err)?;
            }
            other => {
                return Err(AdapterError::Decode(format!(
                    "attribute modifier display type {other} is outside \
                     vanilla's own item attribute modifiers's own display's own type's 0..=2"
                )));
            }
        }
    }
    Ok(())
}

/// Decodes a `minecraft:tool` component (26.2 `vanilla's own tool's own stream codec`).
///
/// Wire shape, in order: a VarInt-counted list of rules, then the default mining
/// speed as an f32, the damage-per-block as a VarInt, and the
/// can-destroy-in-creative flag as a bool. Each rule is a `HolderSet<Block>`,
/// then an optional f32 speed and an optional bool correct-for-drops (both
/// vanilla's optional-value codec, so a present-flag byte then the value).
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

/// Decodes a `HolderSet<Block>` (26.2 `vanilla's holder-set codec(vanilla's own registries's own block)`).
///
/// A single VarInt discriminates: `0` means a named tag follows as an
/// identifier string; any `n > 0` means `n - 1` direct holders follow, each a
/// **bare** `minecraft:block` registry id.
///
/// # The direct holders are *not* `id + 1`
///
/// There are two holder codecs in 26.2 and they differ by exactly one:
/// `vanilla's registry-holder codec(key, directCodec)` reserves `0` for an inline element
/// definition and so writes `id + 1`, while `vanilla's holder-registry codec(key)`
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
        // Vanilla's vanilla's identifier stream codec is an unbounded UTF-8 string, so
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
/// `Holder<Enchantment>` to a VarInt level.
///
/// # The map key is a *bare* registry id, not a holder-encoded one
///
/// This used to read the key as `id + 1` and reject `0` outright as an
/// "unsupported inline holder", as if this component's per-entry `Holder` used
/// the same offset-by-one, either-id-or-inline shape `minecraft:instrument`'s
/// holder does elsewhere in this file. It does not: an enchantment reference is
/// the same bare, unoffset `idMapper` shape `minecraft:rarity`/`minecraft:dye`
/// already use here, with no inline form at all — see
/// `docs/item-data-component-decode.md` for the wire citation this was
/// re-verified against.
///
/// The consequence was two-fold, not one: every *non-zero* wire id decoded to
/// the *wrong* enchantment (off by one, silently — id 12 read as 11), and wire
/// id `0` — a real, ordinary enchantment reference, not an inline marker — hard
/// errored the whole packet. A player or mob wearing an item enchanted with
/// whatever occupies registry id 0 lost their entire equipment list; every
/// other enchanted item showed the wrong enchantment.
fn read_enchantments(reader: &mut Reader<'_>) -> Result<Vec<ItemEnchantment>, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid enchantment count {count}")))?;
    let mut enchantments = Vec::with_capacity(count.min(reader.remaining()).min(64));
    for _ in 0..count {
        let raw = reader.var_i32().map_err(dec_err)?;
        if raw < 0 {
            return Err(AdapterError::Decode(format!(
                "negative enchantment registry id {raw}"
            )));
        }
        let level = reader.var_i32().map_err(dec_err)?;
        let level = u32::try_from(level)
            .map_err(|_| AdapterError::Decode(format!("negative enchantment level {level}")))?;
        enchantments.push(ItemEnchantment { id: raw, level });
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

/// Registry ids of `minecraft:slot_display`, from vanilla's own slot-display
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

/// Walks one `SlotDisplay` (`vanilla's own slot display's own stream codec`), collecting the item ids
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
            // `vanilla's own item stack template's own stream codec`: item id, count, then a
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
            // vanilla's tag-key stream codec is one `Identifier` string. The tag's *members*
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
            // `vanilla's own data component type's own stream codec` is a bare VarInt registry id.
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
            // `vanilla's own trim pattern's own stream codec` is vanilla's registry-holder codec: `0` means an
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

/// Walks one `RecipeDisplay` and returns the item ids of its **result** slot,
/// plus its trailing **station** slot's item ids (the small corner icon —
/// `craftingStation`/`furnace` in vanilla's own field names).
///
/// The result is what a recipe panel and a toast both key on; the station is
/// what a recipe-unlock toast draws as its corner icon (vanilla's own
/// recipe-toast entry).
/// Every `RecipeDisplay` variant's station is its **final** walked `SlotDisplay`
/// — vanilla's own recipe-display registration puts `craftingStation`/`furnace`
/// last in all five variants — so `walked.last()` after the loop below is
/// always it, with no per-kind branch needed. The ingredient slots in
/// between are walked only because they must be consumed. Returns `None`
/// when the walk hit something unmodeled, with the same "abandon the
/// packet" contract as [`read_slot_display`].
///
/// Variant ids are vanilla's own recipe-display registration order:
/// shapeless, shaped, furnace, stonecutter, smithing.
fn read_recipe_display(reader: &mut Reader<'_>) -> Result<Option<(Vec<i32>, Vec<i32>)>, AdapterError> {
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
    let result_items = walked.get(result_index).cloned().unwrap_or_default();
    // The station is always the last `SlotDisplay` walked (see the doc comment):
    // shapeless/shaped push `ingredients.., result, station`, and
    // furnace/stonecutter/smithing push their fixed sequence ending in station.
    let station_items = walked.last().cloned().unwrap_or_default();
    Ok(Some((result_items, station_items)))
}

/// Decodes vanilla's clientbound award-stats packet: a VarInt-counted map of
/// `(stat_type id, value id) -> count`.
///
/// `vanilla's own stat's own stream codec` is `registry(STAT_TYPE).dispatch(Stat::getType,
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

/// Consumes a `HolderSet<T>` and returns the direct entries' registry ids, or an
/// empty list for the tag form.
///
/// vanilla's holder-set codec uses this same wire shape for `Item` ingredients,
/// `Repairable`'s items, and `Equippable`'s entity types: a VarInt where `0`
/// means a tag identifier follows and `n` means `n - 1` explicit bare registry
/// ids. The caller decides whether those ids are worth retaining.
fn read_holder_set(reader: &mut Reader<'_>) -> Result<Vec<i32>, AdapterError> {
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

/// Consumes a `Holder<SoundEvent>` (`vanilla's own sound event's own stream codec`).
///
/// vanilla's registry-holder codec writes `0` for a direct sound definition, whose body
/// is an identifier and an optional fixed-range float; a positive value is a
/// registry reference encoded as `id + 1` and has no body. The decoded sound is
/// intentionally discarded: equippable-slot alignment is the only consumer.
fn read_sound_event_holder(reader: &mut Reader<'_>) -> Result<(), AdapterError> {
    if reader.var_i32().map_err(dec_err)? == 0 {
        reader.string(32767).map_err(dec_err)?;
        if reader.bool().map_err(dec_err)? {
            reader.f32().map_err(dec_err)?;
        }
    }
    Ok(())
}

/// Decodes vanilla's own equippable-component stream codec, retaining only
/// the slot.
///
/// The slot is a plain id-mapper codec, not an enum ordinal. In particular
/// wire id 5 is `OffHand` while enum ordinal 5 is `Head`; map from vanilla's
/// own equipment-slot wire ids explicitly. Vanilla's id-mapper uses its ZERO
/// out-of-bounds strategy, so malformed ids alias `MainHand` just as vanilla
/// does rather than inventing a second validation policy here.
fn read_equippable(reader: &mut Reader<'_>) -> Result<EquipmentSlot, AdapterError> {
    let slot = match reader.var_i32().map_err(dec_err)? {
        0 => EquipmentSlot::MainHand,
        1 => EquipmentSlot::Feet,
        2 => EquipmentSlot::Legs,
        3 => EquipmentSlot::Chest,
        4 => EquipmentSlot::Head,
        5 => EquipmentSlot::OffHand,
        6 => EquipmentSlot::Body,
        7 => EquipmentSlot::Saddle,
        _ => EquipmentSlot::MainHand,
    };
    read_sound_event_holder(reader)?; // equipSound
    if reader.bool().map_err(dec_err)? {
        reader.string(32767).map_err(dec_err)?; // assetId ResourceKey
    }
    if reader.bool().map_err(dec_err)? {
        reader.string(32767).map_err(dec_err)?; // cameraOverlay Identifier
    }
    if reader.bool().map_err(dec_err)? {
        let _allowed_entities = read_holder_set(reader)?;
    }
    for _ in 0..5 {
        reader.bool().map_err(dec_err)?;
    }
    read_sound_event_holder(reader)?; // shearingSound
    Ok(slot)
}

/// Decodes vanilla's clientbound recipe-book-add packet.
///
/// **The trailing `replace: bool` sits after the entry list**, so the list cannot
/// be taken as opaque trailing bytes — the whole reason this packet waited for
/// [`read_slot_display`]. Each entry is a `RecipeDisplayEntry` then an `i8` flags
/// byte (bit 0 notification, bit 1 highlight).
///
/// `RecipeDisplayEntry`'s `group` field is `vanilla's own byte buf codecs's own var int`: a
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
        let Some((result_items, station_items)) = read_recipe_display(&mut reader)? else {
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
                let _ingredient = read_holder_set(&mut reader)?;
            }
        }
        let flags = reader.i8().map_err(dec_err)?;
        entries.push(RecipeBookEntry {
            display_id,
            result_items,
            station_items,
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

/// Decodes vanilla's clientbound update-recipes packet: the property sets, then the
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
        // `SlotDisplay` — a bare display, not a whole `RecipeDisplay`. The input
        // is kept, not discarded: a stonecutter shows only the results reachable
        // from whatever its input slot holds, so a consumer needs the ingredient
        // each result is keyed by, not just the result.
        let input = read_holder_set(&mut reader)?;
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
        stonecutter_results.push((input, display.items));
    }
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(
        ClientEvent::RecipePropertySetsUpdated {
            item_sets,
            stonecutter_results,
        },
    )])
}

/// Decodes vanilla's clientbound merchant-offers packet.
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

/// Decodes vanilla's clientbound show-dialog packet's Play-state form.
///
/// The field is `vanilla's registry-holder codec(vanilla's own registries's own dialog, …)`: a VarInt where `0`
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
/// during Configuration (`vanilla's own map decoration type's own stream codec` is
/// vanilla's holder-registry codec, a bare VarInt registry id). That is why a
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
/// Decodes vanilla's clientbound map-item-data packet (id 51).
///
/// Wire shape, from the record's own `STREAM_CODEC`: a VarInt `MapId`, a `byte`
/// scale, a `bool` locked, `Optional<List<MapDecoration>>`, then
/// the map-patch stream codec's optional.
///
/// Two traps in the patch codec, both confirmed against the decompiled
/// map-saved-data source's own map-patch reader:
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

/// Reads one `ItemStackTemplate` (`vanilla's own item stack template's own stream codec`).
///
/// **Not** the same shape as an `ItemStack`: the template writes the item holder
/// *first* and the count second, where `vanilla's own item stack's own optional stream codec` leads
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

/// Decodes `minecraft:bundle_contents`' payload — vanilla's own
/// bundle-contents stream codec (an item-stack-template codec applied
/// through a list codec, mapped straight onto its own `items` list).
///
/// Each entry is `vanilla's own item stack template's own stream codec`: item id, then count, then a
/// **nested** `DataComponentPatch` — [`read_component_patch`] called
/// recursively, deliberately, since a bundle can legally contain another bundle
/// (`BUNDLE_IN_BUNDLE_WEIGHT`). An unmodeled component inside a *contained*
/// stack is exactly as unrecoverable as one at the top level (its payload has
/// no length prefix either), so it stops the whole bundle list the same way the
/// caller's `other` arm stops the outer patch, rather than hard-failing the
/// packet the way [`read_item_stack_template`]'s advancement-icon caller does —
/// a bundle in a hotbar is not a case this decoder can afford to treat as fatal.
///
/// Bounded at 64 entries defensively: no legal bundle holds anywhere near that
/// many stacks (every contained item costs at least `1/(64*16)` weight against a
/// budget of `1`, and `getNumberOfItemsToShow` itself caps the tooltip at 12), so
/// a declared count above it is a malformed packet, not a large bundle.
fn read_bundle_contents(reader: &mut Reader<'_>) -> Result<(Vec<ItemStack>, bool), AdapterError> {
    let count = read_count(reader, "bundle_contents item")?;
    if count > 64 {
        return Err(AdapterError::Decode(format!(
            "bundle_contents declares {count} items, implausibly many"
        )));
    }
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        let item_id = reader.var_i32().map_err(dec_err)?;
        let name = item_name(item_id)
            .ok_or_else(|| AdapterError::Decode(format!("unknown item registry id {item_id}")))?;
        let item_count = reader.var_i32().map_err(dec_err)?;
        let item_count = u32::try_from(item_count)
            .map_err(|_| AdapterError::Decode(format!("invalid item count {item_count}")))?;
        let (components, complete) = read_component_patch(reader, name)?;
        items.push(ItemStack {
            item: parse_key(name, "item")?,
            count: item_count,
            components,
        });
        if !complete {
            return Ok((items, false));
        }
    }
    Ok((items, true))
}

/// Decodes `minecraft:charged_projectiles`' payload: the same
/// item-then-count-then-recursive-`DataComponentPatch` per-entry shape
/// [`read_bundle_contents`] reads, capped at 1024 entries — the codec's own
/// declared maximum, so a declared count above it is a malformed packet rather
/// than a legitimately large one.
///
/// An unmodeled component inside a *charged* stack is exactly as unrecoverable
/// as one at the top level, so it stops the whole list the same way
/// [`read_bundle_contents`] does rather than hard-failing the packet — a loaded
/// crossbow in a hotbar is not a case this decoder can afford to treat as
/// fatal.
///
/// The reservation is bounded by the bytes actually available, not by the
/// declared count alone: every entry costs at least the two single-byte
/// VarInts an empty patch needs, so no more than `reader.remaining()` of them
/// can ever be produced, and the count is attacker-influenced.
fn read_charged_projectiles(reader: &mut Reader<'_>) -> Result<(Vec<ItemStack>, bool), AdapterError> {
    let count = read_count(reader, "charged_projectiles item")?;
    if count > 1024 {
        return Err(AdapterError::Decode(format!(
            "charged_projectiles declares {count} items, more than the codec's own 1024 cap"
        )));
    }
    let mut items = Vec::with_capacity(count.min(reader.remaining()));
    for _ in 0..count {
        let item_id = reader.var_i32().map_err(dec_err)?;
        let name = item_name(item_id)
            .ok_or_else(|| AdapterError::Decode(format!("unknown item registry id {item_id}")))?;
        let item_count = reader.var_i32().map_err(dec_err)?;
        let item_count = u32::try_from(item_count)
            .map_err(|_| AdapterError::Decode(format!("invalid item count {item_count}")))?;
        let (components, complete) = read_component_patch(reader, name)?;
        items.push(ItemStack {
            item: parse_key(name, "item")?,
            count: item_count,
            components,
        });
        if !complete {
            return Ok((items, false));
        }
    }
    Ok((items, true))
}

/// Decodes vanilla's clientbound update-advancements packet (id 130).
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

#[cfg(test)]
mod dynamic_registry_order {
    //! The three tables in this module that stand in for a **dynamic**
    //! registry's holder-id space share one ordering rule, and it is a rule
    //! the type system cannot state: a dynamic registry has no
    //! `vanilla's static registration` sequence, so its ids come from
    //! `vanilla's resource-manager registry-load task` registering its JSON entries
    //! `.sorted(a by-key comparator())` over the resource `Identifier` —
    //! and `vanilla's identifier comparator` compares **path first**. Every entry in
    //! these three registries is `minecraft:`, so that reduces to plain
    //! ascending order of the file stems.
    //!
    //! All three tables were once transcribed from the matching
    //! `*.bootstrap` datagen routine instead, which is a different order that
    //! runs in no server. A comment stating the rule is what let that stand;
    //! these are the mechanical form.

    use super::{BANNER_PATTERN_IDS, TRIM_MATERIAL_IDS, TRIM_PATTERN_IDS};

    /// Collects *every* out-of-order neighbour rather than asserting inside
    /// the loop — a table transcribed from the wrong source is wrong in many
    /// places at once, and a gate that aborts at the first one reports a
    /// single pair where the finding is "this whole table came from
    /// somewhere else".
    fn out_of_order(table: &[&str]) -> Vec<String> {
        table
            .windows(2)
            .filter(|w| w[0] >= w[1])
            .map(|w| format!("{:?} is not before {:?}", w[0], w[1]))
            .collect()
    }

    #[test]
    fn trim_material_ids_are_sorted_by_resource_path() {
        assert_eq!(
            TRIM_MATERIAL_IDS.len(),
            11,
            "26.2 ships 11 data/minecraft/trim_material/*.json entries"
        );
        assert!(
            out_of_order(TRIM_MATERIAL_IDS).is_empty(),
            "{:?}",
            out_of_order(TRIM_MATERIAL_IDS)
        );
    }

    #[test]
    fn trim_pattern_ids_are_sorted_by_resource_path() {
        assert_eq!(
            TRIM_PATTERN_IDS.len(),
            18,
            "26.2 ships 18 data/minecraft/trim_pattern/*.json entries"
        );
        assert!(
            out_of_order(TRIM_PATTERN_IDS).is_empty(),
            "{:?}",
            out_of_order(TRIM_PATTERN_IDS)
        );
    }

    #[test]
    fn banner_pattern_ids_are_sorted_by_resource_path() {
        assert_eq!(
            BANNER_PATTERN_IDS.len(),
            43,
            "26.2 ships 43 data/minecraft/banner_pattern/*.json entries"
        );
        assert!(
            out_of_order(BANNER_PATTERN_IDS).is_empty(),
            "{:?}",
            out_of_order(BANNER_PATTERN_IDS)
        );
    }
}
