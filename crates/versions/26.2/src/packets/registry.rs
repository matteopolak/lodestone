//! Clientbound `registry_data` (Configuration) for protocol 776, and the two
//! registries this client actually reads off it.
//!
//! # What arrives, and why it was ignored for so long
//!
//! During Configuration the server sends one `registry_data` packet **per
//! synchronized registry** — measured 29 on the creative oracle, matching
//! vanilla's own synchronized-registries list exactly. This
//! client used to drop every one of them on the floor, so anything the server
//! declares by registry rather than by name had to be hardcoded: chunk column
//! heights, sky light, the day/night clock.
//!
//! Two surprises from the measured set, both of which would break a lookup by
//! guessed name:
//!
//! * The biome registry arrives as **`minecraft:worldgen/biome`**, not
//!   `minecraft:biome` — vanilla's own biome registry key carries the
//!   `worldgen/` prefix.
//! * `minecraft:dimension_type` entries arrive **alphabetically**:
//!   `overworld`, `overworld_caves`, `the_end`, `the_nether` — so holder ids are
//!   `0, 1, 2, 3` in that order and `the_nether` is **3**, not 1. Never assume a
//!   holder id; resolve it.
//!
//! # Wire format
//!
//! From vanilla's own clientbound registry-data packet stream codec
//! (behavioural reference only, never transliterated):
//!
//! ```text
//! registry : Identifier                     -- e.g. "minecraft:dimension_type"
//! entries  : VarInt count, then per entry:
//!     id   : Identifier                     -- e.g. "minecraft:overworld"
//!     data : bool, then network NBT if true  -- Optional<Tag>
//! ```
//!
//! **The entry order *is* the registry order**, and the registry order *is* the
//! holder-id space every other packet uses. That is the whole reason this decode
//! is worth having: `login`/`respawn` carry `dimension_type` as a bare VarInt
//! index, and `set_time` keys its clock map by a bare `world_clock` VarInt index
//! (see [`crate::packets::time::ClockUpdate::holder_id`]). Without this packet
//! those integers are unresolvable, which is exactly why they were being routed
//! around by matching on the *dimension* (level) name instead.
//!
//! # `data` is an `Option`, and for us it is always `Some`
//!
//! Vanilla's own registry pack-sync routine elides an entry's contents when the
//! entry came from a data pack the client said it already knows
//! (its own can-skip-contents check). Our join replies to `select_known_packs` with an **empty**
//! list (`V770Adapter::handle_configuration`), so vanilla's own
//! notion of packs the client already knows is empty
//! server-side and nothing is ever elided — every entry we receive carries full
//! NBT. Measured on the creative oracle: 4 of 4 `dimension_type` entries and 2 of
//! 2 `world_clock` entries carried data (see `tests/live_registry_data.rs`).
//! The `Option` is still decoded properly rather than assumed, because the day
//! we do claim a known pack the elision starts immediately and a wrong guess
//! here would desynchronise the *whole* packet, not just one field.
//!
//! # Scope: what is parsed, and what is only counted
//!
//! [`ClientRegistries`] keeps typed [`DimensionType`]s, the ordered
//! `world_clock` names, and one attribute off each biome
//! ([`ClientRegistries::biome_sky_colors`]). For every other registry it keeps
//! only the ordered **names** — the id ↔ name mapping, the part that is
//! universally useful and costs a `Vec<String>`. Their NBT payloads are
//! **dropped**, not retained. When something does read one (damage types for
//! `EntityDamaged`, chat types, trim patterns), it parses out of this same
//! decode — add a typed arm beside [`DimensionType`], do not grow a generic
//! `Nbt` cache.
//!
//! The biome arm is deliberately *one field*, not a `Biome` record: the whole
//! registry is ~66 entries of deep compounds (climate settings, mob spawns,
//! carvers) and the sky disc reads exactly one string out of it. It is lifted
//! here rather than reconstructed from our jar because the server is
//! authoritative — a data pack can reorder the registry, rename a biome, or
//! change its colour, and shipping the value at the holder id is correct for
//! all three.

use std::collections::HashMap;

use lodestone_core::{Ctx, Decode, Error, Nbt, Reader, Result, read_network_nbt};

/// Maximum characters in an `Identifier` on the wire (vanilla's own limit).
const MAX_IDENTIFIER_CHARS: usize = 32767;

/// Sanity cap on entries in one `registry_data` packet. Vanilla's largest
/// synchronized registry (`minecraft:biome`) is in the dozens; this only exists
/// so a hostile length prefix cannot make us pre-allocate.
const MAX_ENTRIES: usize = 65536;

/// One packed registry entry: its resource id, and its contents when the server
/// did not elide them.
#[derive(Debug, Clone, PartialEq)]
pub struct PackedRegistryEntry {
    /// The entry's resource id, e.g. `minecraft:overworld`.
    pub id: String,
    /// The entry's serialized value, or `None` when the server elided it
    /// because the client claimed the data pack it came from.
    pub data: Option<Nbt>,
}

/// Clientbound `registry_data` packet body: one registry, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct RegistryData {
    /// The registry being synchronized, e.g. `minecraft:dimension_type`.
    pub registry: String,
    /// The registry's entries **in registry order** — index `i` is holder id
    /// `i`.
    pub entries: Vec<PackedRegistryEntry>,
}

impl Decode for RegistryData {
    fn decode(r: &mut Reader<'_>, ctx: Ctx) -> Result<Self> {
        let _ = ctx;
        let registry = r.string(MAX_IDENTIFIER_CHARS)?;
        let count = r.var_i32()?;
        let count = usize::try_from(count).map_err(|_| Error::NegativeLength(count))?;
        if count > MAX_ENTRIES {
            return Err(Error::LimitExceeded {
                limit: MAX_ENTRIES,
                actual: count,
            });
        }
        // `min` rather than the raw count: the cap above already rejects absurd
        // values, this keeps a merely-large-but-legal count from pre-allocating.
        let mut entries = Vec::with_capacity(count.min(256));
        for _ in 0..count {
            let id = r.string(MAX_IDENTIFIER_CHARS)?;
            let data = if r.bool()? {
                Some(read_network_nbt(r)?)
            } else {
                None
            };
            entries.push(PackedRegistryEntry { id, data });
        }
        Ok(Self { registry, entries })
    }
}

/// The `minecraft:dimension_type` fields this client can act on.
///
/// # Which fields, and why not all of them
///
/// 26.2's `DimensionType` record has 16 components. These eleven are the ones a
/// consumer exists (or is one step away) for; the five omitted ones —
/// `infiniburn`, `monster_settings`, `skybox`, `cardinal_light`, `timelines` —
/// are each a nested registry-reference structure whose only consumers are
/// systems this client does not have. They stay in the raw NBT and are dropped.
/// Add a field here when something reads it, not before.
///
/// `attributes` is no longer fully in that list: it is a generic, many-keyed
/// attribute map (vanilla's own environment-attributes structure; fog colour,
/// ambient sounds, bed rules, dripstone
/// particles, …) and only one key of it — `minecraft:visual/ambient_light_color`
/// — has a consumer, so only that one key is lifted out (see
/// [`Self::ambient_light_color`]). The rest of the compound stays in the raw
/// NBT and is dropped, exactly like the five fields above.
///
/// # Two field names that are *not* what older records say
///
/// * The key is **`has_skylight`**, one word — not `has_sky_light`. Vanilla's
///   accessor uses a different name than the codec field, which is `has_skylight`, so code
///   ported from the accessor name silently finds nothing.
/// * There is **no `bed_works`** and no `respawn_anchor_works` in 26.2. They
///   moved into `attributes` as `minecraft:gameplay/bed_rule` and
///   `minecraft:gameplay/respawn_anchor_works` (see
///   `.cache/mc/26.2/client-src/data/minecraft/dimension_type/overworld.json`).
///   Anything written before 26.2 that lists `bed_works` as a dimension-type
///   field is describing an older game.
///
/// Also note `has_fixed_time` is a **bool** here (vanilla's own optional
/// bool-field codec),
/// not the pre-26.2 `Optional<Long> fixed_time`. It gates
/// vanilla's own dark-outside/night checks; nothing in this client reads it yet, but it
/// is one byte and it is the field a future "the Nether has no night" fix needs.
#[derive(Debug, Clone, PartialEq)]
pub struct DimensionType {
    /// Whether this dimension's time of day is fixed (vanilla's own
    /// has-fixed-time check).
    /// Absent on the wire means `false`.
    pub has_fixed_time: bool,
    /// Whether columns in this dimension carry sky light at all.
    ///
    /// **`false` in the Nether, `true` in the End** — the End is not "the other
    /// non-overworld"; see `lodestone_shell::mesher::sky_default_for_dimension`,
    /// which this field exists to replace a name match in.
    pub has_skylight: bool,
    /// Whether the dimension has a solid ceiling (the Nether).
    pub has_ceiling: bool,
    /// Whether the ender dragon fight runs here.
    pub has_ender_dragon_fight: bool,
    /// Movement scale relative to the overworld — `8.0` in the Nether. Used for
    /// portal coordinate translation (vanilla's own teleportation-scale getter).
    pub coordinate_scale: f64,
    /// Lowest world-`y` a column stores (`-64` overworld, `0` Nether/End).
    pub min_y: i32,
    /// Total column height in blocks (`384` overworld, `256` Nether/End).
    /// Always a multiple of 16 and at least 16, enforced by vanilla's own
    /// record constructor.
    pub height: i32,
    /// Highest `y` a portal or a bed may place the player at; `128` in the
    /// Nether against a `height` of `256`.
    pub logical_height: i32,
    /// Baseline light every block receives regardless of exposure — `0.0`
    /// overworld, `0.1` Nether, `0.25` End.
    pub ambient_light: f32,
    /// `attributes`' `minecraft:visual/ambient_light_color`, packed `0xRRGGBB` —
    /// the colour `lightmap.fsh` seeds its accumulator with before either light
    /// half is added (vanilla's own lightmap-render state extractor reads this
    /// from its own environment-attributes table). **Not**
    /// the same quantity as [`Self::ambient_light`] above, which only ever
    /// blends a *lerp fraction* into vanilla's own lightmap-brightness
    /// calculation and is not what the GPU lightmap texture actually uses.
    ///
    /// Grey `0x0a0a0a` in the overworld, warm brown `0x302821` in the Nether,
    /// sage `0x3f473f` in the End (`.cache/mc/26.2/client-src/data/minecraft/
    /// dimension_type/*.json`) — **not** a small, dark-in-every-dimension
    /// constant: the Nether's and End's floors are both markedly *brighter*
    /// than the overworld's grey, which is why hardcoding the overworld's value
    /// everywhere under-lit both of them.
    ///
    /// `None` when the entry's `attributes` compound is absent or does not
    /// carry this key — vanilla's own registered default for the attribute is
    /// black (`-16777216`), and every built-in dimension type sets it
    /// explicitly, so `None` in practice means a non-vanilla dimension that
    /// genuinely did not set one, not a decode failure.
    pub ambient_light_color: Option<u32>,
    /// The `minecraft:world_clock` entry this dimension's day/night cycle
    /// follows, when it has one.
    ///
    /// `minecraft:overworld` in the overworld, **`minecraft:the_end` in the
    /// End**, and **absent in the Nether** (which has fixed time and therefore
    /// no clock of its own). This is what turns `set_time`'s bare clock holder
    /// id into the right clock — see [`ClientRegistries::world_clock_id`].
    pub default_clock: Option<String>,
}

impl DimensionType {
    /// Number of 16-tall block sections in a column of this dimension.
    #[must_use]
    pub fn section_count(&self) -> usize {
        // `height` is a multiple of 16 in `[16, Y_SIZE]` by vanilla's own record
        // invariant; clamping at 0 keeps a hostile server from producing a
        // negative cast rather than trusting the invariant.
        usize::try_from(self.height.max(0)).unwrap_or(0) / 16
    }

    /// Parses a `minecraft:dimension_type` entry's network NBT.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Custom`] when the payload is not a compound, or when a
    /// required field is missing or holds an unusable tag. Vanilla's codec is
    /// equally strict about the required fields (`fieldOf`, not
    /// `optionalFieldOf`), so a missing `has_skylight` really is a malformed
    /// entry and not a defaulting opportunity.
    pub fn from_nbt(value: &Nbt) -> Result<Self> {
        let Nbt::Compound(fields) = value else {
            return Err(Error::Custom(
                "dimension_type entry is not an NBT compound".to_owned(),
            ));
        };
        Ok(Self {
            has_fixed_time: optional_bool(fields, "has_fixed_time")?.unwrap_or(false),
            has_skylight: required_bool(fields, "has_skylight")?,
            has_ceiling: required_bool(fields, "has_ceiling")?,
            has_ender_dragon_fight: required_bool(fields, "has_ender_dragon_fight")?,
            coordinate_scale: required_f64(fields, "coordinate_scale")?,
            min_y: required_i32(fields, "min_y")?,
            height: required_i32(fields, "height")?,
            logical_height: required_i32(fields, "logical_height")?,
            ambient_light: required_f32(fields, "ambient_light")?,
            ambient_light_color: dimension_ambient_light_color(fields),
            default_clock: match field(fields, "default_clock") {
                None => None,
                Some(Nbt::String(name)) => Some(name.clone()),
                Some(_) => {
                    return Err(Error::Custom(
                        "dimension_type default_clock is not a string".to_owned(),
                    ));
                }
            },
        })
    }
}

/// The climate fields `lodestone_render::precipitation_for_temperature` needs
/// to decide rain versus snow per biome, plus `downfall` for a future
/// `lodestone_assets::BiomeTint` implementor.
///
/// Read from vanilla's own biome climate-settings codec (confirmed against
/// the decompiled 26.2 biome source), the same
/// top-level compound `has_precipitation`/`temperature`/`downfall` live in —
/// **not** under `attributes` like [`biome_sky_color`]'s field. This is a
/// **data-pack** registry like the rest of the biome table: a pack can change
/// a biome's climate, so nothing here is hardcoded from our own jar.
///
/// # What is deliberately not modelled
///
/// `temperature_modifier` (`"none"`/`"frozen"`) and the per-block height
/// falloff above `sea_level + 17` are both real inputs to vanilla's exact
/// height-adjusted-temperature calculation (confirmed against the decompiled
/// biome source) and neither is decoded
/// here — this is the same documented approximation
/// `docs/worldgen-biomes.md`'s `cold_enough_to_snow` gotcha already describes
/// for the *server*-side climate table, carried over to the client-side one
/// rather than introduced fresh. `temperature` is the biome's *declared*
/// (sea-level, unmodified) value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BiomeClimate {
    /// `has_precipitation` — `false` means the biome never rains or snows at
    /// all (deserts, most Nether/End biomes), regardless of temperature.
    pub has_precipitation: bool,
    /// `temperature`, declared (not height-adjusted). `>= 0.15` is rain,
    /// otherwise snow, when `has_precipitation` is `true`
    /// (vanilla's own warm-enough-to-rain threshold, confirmed against the
    /// decompiled biome source).
    pub temperature: f32,
    /// `downfall`, `0.0..=1.0`. Feeds the grass/foliage colormap sample
    /// alongside `temperature`; not consulted for precipitation.
    pub downfall: f32,
}

/// Reads a biome entry's `has_precipitation`/`temperature`/`downfall` triple
/// off its network NBT, per [`BiomeClimate`].
///
/// `None` — never an error — when any of the three required fields is
/// missing or the wrong tag, or the value is not a compound at all: an
/// unparseable *climate* still leaves the entry's index holding its place (see
/// [`ClientRegistries::biome_climates`]), exactly as an unparseable sky colour
/// does for [`ClientRegistries::biome_sky_colors`]. Vanilla's own codec has all
/// three as `fieldOf` (required), so this is genuinely "could not parse", not
/// "declines to answer" the way an absent `sky_color` is.
fn biome_climate(value: &Nbt) -> Option<BiomeClimate> {
    let Nbt::Compound(fields) = value else {
        return None;
    };
    Some(BiomeClimate {
        has_precipitation: required_bool(fields, "has_precipitation").ok()?,
        temperature: required_f32(fields, "temperature").ok()?,
        downfall: required_f32(fields, "downfall").ok()?,
    })
}

/// Everything this client keeps from the Configuration `registry_data` stream.
///
/// One instance per connection, folded packet by packet via [`Self::apply`]. A
/// second `registry_data` for the same registry **replaces** it: vanilla sends
/// each one exactly once per Configuration phase, and re-entering Configuration
/// (`start_configuration`, i.e. a server switching the player between worlds)
/// resends the set, so replacing is the correct merge and appending would double
/// every holder id.
#[derive(Debug, Clone, Default)]
pub struct ClientRegistries {
    /// `minecraft:dimension_type`, in registry order: index `i` is holder id
    /// `i`. Entries whose NBT failed to parse (or was elided) hold `None` so the
    /// *indices* of the entries around them stay correct — dropping a bad entry
    /// would shift every later holder id by one, which is far worse than one
    /// unresolvable dimension.
    dimension_types: Vec<(String, Option<DimensionType>)>,
    /// `minecraft:world_clock` names, in registry order. The values are unit
    /// compounds (`WorldClock` is `record WorldClock()`), so the name *is* the
    /// whole content.
    world_clocks: Vec<String>,
    /// `minecraft:worldgen/biome` sky colours, in registry order: index `i` is
    /// holder id `i`, which is the integer a chunk's biome palette stores.
    ///
    /// `None` where the biome declares no `minecraft:visual/sky_color` (the ten
    /// Nether and End biomes) or where the entry was elided/unparseable — and,
    /// exactly as for [`Self::dimension_types`], a `None` **holds its place** so
    /// the indices around it stay correct.
    ///
    /// The *names* of these entries still live in [`Self::other`] like every
    /// other unmodelled registry; only this one attribute is lifted out, because
    /// only this one has a consumer (the sky disc's tint).
    biome_sky_colors: Vec<Option<u32>>,
    /// `minecraft:worldgen/biome` climates (`has_precipitation`/`temperature`/
    /// `downfall`), in registry order: index `i` is holder id `i`, exactly as
    /// for [`Self::biome_sky_colors`]. `None` holds its place for the same
    /// reason — an unparseable or elided entry must not shift every later
    /// holder id.
    ///
    /// Unlike sky colour, every 26.2 biome declares a climate (vanilla's codec
    /// has all three fields required, `fieldOf` not `optionalFieldOf`), so a
    /// `None` here should only ever mean "elided" or "malformed", never "this
    /// biome legitimately has none".
    biome_climates: Vec<Option<BiomeClimate>>,
    /// Ordered entry names for every other synchronized registry, keyed by
    /// registry id. Names only — see this module's scope note.
    other: HashMap<String, Vec<String>>,
}

impl ClientRegistries {
    /// Registry id of the dimension-type registry.
    pub const DIMENSION_TYPE: &'static str = "minecraft:dimension_type";
    /// Registry id of the world-clock registry.
    pub const WORLD_CLOCK: &'static str = "minecraft:world_clock";
    /// Registry id of the biome registry.
    ///
    /// **`minecraft:worldgen/biome`, not `minecraft:biome`** — vanilla's own biome registry key's
    /// key carries the `worldgen/` prefix, and a lookup by the guessed short name
    /// silently finds nothing. See this module's header.
    pub const BIOME: &'static str = "minecraft:worldgen/biome";

    /// Folds one decoded `registry_data` packet in.
    ///
    /// Never fails: a registry we do not model, or an entry whose NBT does not
    /// parse, is recorded as such rather than propagated. A malformed
    /// *dimension type* must not disconnect the client — the connection is
    /// perfectly usable with a name-matched fallback shape, which is what the
    /// adapter does when a lookup returns `None`.
    pub fn apply(&mut self, data: RegistryData) {
        match data.registry.as_str() {
            Self::DIMENSION_TYPE => {
                self.dimension_types = data
                    .entries
                    .into_iter()
                    .map(|entry| {
                        let parsed = entry
                            .data
                            .as_ref()
                            .and_then(|nbt| DimensionType::from_nbt(nbt).ok());
                        (entry.id, parsed)
                    })
                    .collect();
            }
            Self::WORLD_CLOCK => {
                self.world_clocks = data.entries.into_iter().map(|entry| entry.id).collect();
            }
            _ => {
                if data.registry == Self::BIOME {
                    self.biome_sky_colors = data
                        .entries
                        .iter()
                        .map(|entry| entry.data.as_ref().and_then(biome_sky_color))
                        .collect();
                    self.biome_climates = data
                        .entries
                        .iter()
                        .map(|entry| entry.data.as_ref().and_then(biome_climate))
                        .collect();
                }
                self.other.insert(
                    data.registry,
                    data.entries.into_iter().map(|entry| entry.id).collect(),
                );
            }
        }
    }

    /// The dimension type at holder id `id` — the integer `login` and `respawn`
    /// carry — with its registry name.
    #[must_use]
    pub fn dimension_type(&self, id: i32) -> Option<(&str, &DimensionType)> {
        let index = usize::try_from(id).ok()?;
        let (name, parsed) = self.dimension_types.get(index)?;
        Some((name.as_str(), parsed.as_ref()?))
    }

    /// The dimension type registered under `name`, e.g. `minecraft:overworld`.
    ///
    /// Note this is a **dimension-type** id, not a level id: a server may name a
    /// level `mypack:mine` while its type is `minecraft:overworld`.
    #[must_use]
    pub fn dimension_type_by_name(&self, name: &str) -> Option<&DimensionType> {
        self.dimension_types
            .iter()
            .find(|(id, _)| id == name)
            .and_then(|(_, parsed)| parsed.as_ref())
    }

    /// Holder id of the `minecraft:world_clock` entry named `name`, i.e. the key
    /// `set_time`'s clock map uses.
    ///
    /// This is the resolution `SetTime::day_clock`'s "lowest holder id"
    /// heuristic was standing in for: vanilla registers `minecraft:overworld`
    /// first, so the heuristic silently returns the overworld's clock **in every
    /// dimension**, including the End, whose own clock is holder `1`.
    #[must_use]
    pub fn world_clock_id(&self, name: &str) -> Option<i32> {
        let index = self.world_clocks.iter().position(|clock| clock == name)?;
        i32::try_from(index).ok()
    }

    /// Whether any `registry_data` has been folded in yet. `false` means the
    /// caller must use its no-registries fallback: the fields are absent, not
    /// empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dimension_types.is_empty() && self.world_clocks.is_empty() && self.other.is_empty()
    }

    /// Ordered entry names of a registry this client does not model, for
    /// diagnostics and for the next consumer that needs an id ↔ name mapping.
    #[must_use]
    pub fn entry_names(&self, registry: &str) -> Option<&[String]> {
        match registry {
            Self::DIMENSION_TYPE => None,
            Self::WORLD_CLOCK => Some(&self.world_clocks),
            other => self.other.get(other).map(Vec::as_slice),
        }
    }

    /// Every biome's `minecraft:visual/sky_color` in registry order, packed
    /// `0x00RR_GGBB` in **sRGB bytes** (the space vanilla's own colour-multiply
    /// helper works in — never linearise before the day/night multiply, see
    /// `lodestone_render::SkyFrame`).
    ///
    /// Index `i` is holder id `i`, which is exactly the integer a chunk
    /// section's biome palette stores, so a consumer indexes this with
    /// `ChunkSection::biome_at_block`'s return value and nothing else. Empty
    /// before any `registry_data` arrives.
    ///
    /// # Why the colour travels and the name does not
    ///
    /// This is the whole reason the biome registry is worth a typed arm: the
    /// server is authoritative about it. A data pack may reorder the registry,
    /// rename a biome, or change its sky colour, and every one of those is
    /// carried correctly by shipping the *value* at the holder id. A
    /// name → colour table built from our jar would be wrong on all three, and
    /// would have to be re-derived every version.
    #[must_use]
    pub fn biome_sky_colors(&self) -> &[Option<u32>] {
        &self.biome_sky_colors
    }

    /// Every biome's climate (`has_precipitation`/`temperature`/`downfall`) in
    /// registry order.
    ///
    /// Index `i` is holder id `i`, the same integer [`biome_sky_colors`]
    /// indexes with and the same one a chunk section's biome palette stores
    /// (`ChunkSection::biome_at_block`) — a consumer needs no second lookup to
    /// go from "which biome is this block in" to "what does it do in the
    /// weather". Empty before any `registry_data` arrives.
    ///
    /// [`biome_sky_colors`]: Self::biome_sky_colors
    #[must_use]
    pub fn biome_climates(&self) -> &[Option<BiomeClimate>] {
        &self.biome_climates
    }

    /// Number of dimension types received.
    #[must_use]
    pub fn dimension_type_count(&self) -> usize {
        self.dimension_types.len()
    }

    /// Registry names and entry counts, in fold order per registry family, for
    /// the one-line join log.
    #[must_use]
    pub fn summary(&self) -> Vec<(String, usize)> {
        let mut out = Vec::with_capacity(self.other.len() + 2);
        if !self.dimension_types.is_empty() {
            out.push((Self::DIMENSION_TYPE.to_owned(), self.dimension_types.len()));
        }
        if !self.world_clocks.is_empty() {
            out.push((Self::WORLD_CLOCK.to_owned(), self.world_clocks.len()));
        }
        out.extend(
            self.other
                .iter()
                .map(|(registry, entries)| (registry.clone(), entries.len())),
        );
        out
    }
}

/// Reads `attributes."minecraft:visual/sky_color"` out of one biome entry's
/// network NBT, as packed `0x00RR_GGBB`.
///
/// `None` — never an error — when the biome simply does not declare one. 56 of
/// 26.2's 66 biomes do; the ten that do not are exactly the Nether and End
/// biomes, whose dimensions draw no sky disc at all, so an absent value is
/// ordinary data rather than a malformed entry.
///
/// # The tag is a string, and there are two shapes of it
///
/// Vanilla's own sky-color attribute is registered with its own
/// string-or-rgb-color codec — an alternative-of (hex-color-string,
/// int-rgb-color). Vanilla *encodes* through
/// the first alternative, so what actually arrives is the NBT string
/// `"#78a7ff"`, not an int; the int form is accepted anyway because it is a
/// legal alternative a data pack may author.
///
/// The second shape is the modifier form. Vanilla's own environment-attribute
/// map entry is
/// an either-codec (valueCodec, fullCodec): a plain `override` collapses to the
/// bare value, but any other modifier serialises as
/// `{ modifier: "…", argument: <value> }`. No vanilla biome uses a modifier for
/// `sky_color` (`swamp`'s `water_fog_end_distance` is the shape's only vanilla
/// user), but a data pack may, and reading the bare tag alone would silently
/// return `None` for it.
fn biome_sky_color(value: &Nbt) -> Option<u32> {
    let Nbt::Compound(fields) = value else {
        return None;
    };
    let Nbt::Compound(attributes) = field(fields, "attributes")? else {
        return None;
    };
    match field(attributes, "minecraft:visual/sky_color")? {
        Nbt::String(hex) => parse_hex_rgb(hex),
        // `ARGB::opaque` puts `0xFF` in the top byte; the palette is RGB only.
        Nbt::Int(packed) => Some((*packed as u32) & 0x00FF_FFFF),
        Nbt::Compound(entry) => match field(entry, "argument")? {
            Nbt::String(hex) => parse_hex_rgb(hex),
            Nbt::Int(packed) => Some((*packed as u32) & 0x00FF_FFFF),
            _ => None,
        },
        _ => None,
    }
}

/// Reads the ambient-light colour nested below `attributes` at
/// `minecraft:visual/ambient_light_color`. Like [`biome_sky_color`], it
/// accepts a hexadecimal string on the wire, an integer, or the
/// `{ modifier, argument }` form a data pack may author.
///
/// Confirmed against a real captured `dimension_type` payload
/// (`tests/fixtures/registry_data_dimension_type.hex`): the Nether's entry
/// carries `0x302821` at this key. Its wire NBT string spells the same value
/// as `#` followed by `302821`.
fn dimension_ambient_light_color(fields: &[(String, Nbt)]) -> Option<u32> {
    let Nbt::Compound(attributes) = field(fields, "attributes")? else {
        return None;
    };
    match field(attributes, "minecraft:visual/ambient_light_color")? {
        Nbt::String(hex) => parse_hex_rgb(hex),
        Nbt::Int(packed) => Some((*packed as u32) & 0x00FF_FFFF),
        Nbt::Compound(entry) => match field(entry, "argument")? {
            Nbt::String(hex) => parse_hex_rgb(hex),
            Nbt::Int(packed) => Some((*packed as u32) & 0x00FF_FFFF),
            _ => None,
        },
        _ => None,
    }
}

/// Parses vanilla's own six-digit hex-color form: a leading `#` and exactly
/// six hex digits. Both requirements are vanilla's own, so
/// anything else is a value we should decline rather than guess at.
fn parse_hex_rgb(text: &str) -> Option<u32> {
    let digits = text.strip_prefix('#')?;
    if digits.len() != 6 {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

fn field<'a>(fields: &'a [(String, Nbt)], name: &str) -> Option<&'a Nbt> {
    fields
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

/// Reads a required NBT boolean. Booleans are `Byte` tags; any nonzero byte is
/// `true`, matching `vanilla's own codec's own bool` over `NbtOps`.
fn required_bool(fields: &[(String, Nbt)], name: &str) -> Result<bool> {
    optional_bool(fields, name)?.ok_or_else(|| missing(name))
}

fn optional_bool(fields: &[(String, Nbt)], name: &str) -> Result<Option<bool>> {
    match field(fields, name) {
        None => Ok(None),
        Some(Nbt::Byte(value)) => Ok(Some(*value != 0)),
        Some(_) => Err(wrong_tag(name, "byte")),
    }
}

/// Reads a required integral field. `Byte`/`Short`/`Int` are all accepted
/// because `NbtOps::getNumberValue` is width-agnostic — a server (or a data
/// pack) writing `min_y` as a short is legal input, not a protocol error.
fn required_i32(fields: &[(String, Nbt)], name: &str) -> Result<i32> {
    match field(fields, name).ok_or_else(|| missing(name))? {
        Nbt::Byte(value) => Ok(i32::from(*value)),
        Nbt::Short(value) => Ok(i32::from(*value)),
        Nbt::Int(value) => Ok(*value),
        _ => Err(wrong_tag(name, "int")),
    }
}

/// Reads a required floating field, accepting any numeric tag for the same
/// reason [`required_i32`] does.
fn required_f64(fields: &[(String, Nbt)], name: &str) -> Result<f64> {
    match field(fields, name).ok_or_else(|| missing(name))? {
        Nbt::Double(value) => Ok(*value),
        Nbt::Float(value) => Ok(f64::from(*value)),
        Nbt::Int(value) => Ok(f64::from(*value)),
        Nbt::Byte(value) => Ok(f64::from(*value)),
        Nbt::Short(value) => Ok(f64::from(*value)),
        _ => Err(wrong_tag(name, "double")),
    }
}

fn required_f32(fields: &[(String, Nbt)], name: &str) -> Result<f32> {
    #[allow(clippy::cast_possible_truncation)]
    Ok(required_f64(fields, name)? as f32)
}

fn missing(name: &str) -> Error {
    Error::Custom(format!("dimension_type entry is missing {name}"))
}

fn wrong_tag(name: &str, expected: &str) -> Error {
    Error::Custom(format!(
        "dimension_type field {name} is not a(n) {expected}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_core::Writer;

    /// Encodes a `registry_data` body the way the *server* does, for the
    /// negative/edge cases where no captured payload exists. The load-bearing
    /// decode assertions live in `tests/registry_data.rs`, against bytes
    /// captured off a real 26.2 server — a self-encoded fixture cannot prove a
    /// layout, only that we are self-consistent (`CLAUDE.md`, evidence
    /// standards).
    fn encode(registry: &str, entries: &[(&str, Option<&[u8]>)]) -> Vec<u8> {
        let mut w = Writer::default();
        w.string(registry);
        w.var_i32(i32::try_from(entries.len()).expect("test entry count"));
        for (id, data) in entries {
            w.string(id);
            match data {
                Some(bytes) => {
                    w.bool(true);
                    w.bytes(bytes);
                }
                None => w.bool(false),
            }
        }
        w.into_vec()
    }

    #[test]
    fn an_elided_entry_still_holds_its_place_in_the_holder_id_space() {
        // The failure this rules out: skipping a data-less entry, which would
        // shift every later holder id by one and silently mis-resolve every
        // `login.dimension_type` after it.
        let bytes = encode(
            "minecraft:dimension_type",
            &[("mypack:first", None), ("mypack:second", None)],
        );
        let mut reader = Reader::new(&bytes);
        let data = RegistryData::decode(&mut reader, Ctx { version: 776 }).expect("decodes");
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(data.entries.len(), 2);
        assert!(data.entries.iter().all(|entry| entry.data.is_none()));

        let mut registries = ClientRegistries::default();
        registries.apply(data);
        assert_eq!(registries.dimension_type_count(), 2);
        // Both are unresolvable, but the *count* is right, so a later entry's id
        // is not stolen by an earlier elision.
        assert!(registries.dimension_type(0).is_none());
        assert!(registries.dimension_type(1).is_none());
    }

    #[test]
    fn a_resent_registry_replaces_rather_than_appends() {
        // `start_configuration` re-sends the whole set. Appending would put the
        // second copy's `minecraft:overworld` at holder id 2 and leave the stale
        // one answering id 0.
        let mut registries = ClientRegistries::default();
        for _ in 0..2 {
            registries.apply(RegistryData {
                registry: "minecraft:world_clock".to_owned(),
                entries: vec![
                    PackedRegistryEntry {
                        id: "minecraft:overworld".to_owned(),
                        data: None,
                    },
                    PackedRegistryEntry {
                        id: "minecraft:the_end".to_owned(),
                        data: None,
                    },
                ],
            });
        }
        assert_eq!(registries.world_clock_id("minecraft:overworld"), Some(0));
        assert_eq!(registries.world_clock_id("minecraft:the_end"), Some(1));
        assert_eq!(registries.entry_names("minecraft:world_clock").map(<[String]>::len), Some(2));
    }

    #[test]
    fn a_negative_entry_count_is_rejected_rather_than_wrapped() {
        let mut w = Writer::default();
        w.string("minecraft:world_clock");
        w.var_i32(-1);
        let bytes = w.into_vec();
        let err = RegistryData::decode(&mut Reader::new(&bytes), Ctx { version: 776 })
            .expect_err("a negative count must not decode");
        assert!(matches!(err, Error::NegativeLength(-1)), "got {err:?}");
    }

    #[test]
    fn an_unmodelled_registry_keeps_its_names_and_drops_its_payloads() {
        let mut registries = ClientRegistries::default();
        registries.apply(RegistryData {
            registry: "minecraft:damage_type".to_owned(),
            entries: vec![PackedRegistryEntry {
                id: "minecraft:in_fire".to_owned(),
                data: Some(Nbt::Compound(vec![(
                    "exhaustion".to_owned(),
                    Nbt::Float(0.0),
                )])),
            }],
        });
        assert_eq!(
            registries.entry_names("minecraft:damage_type"),
            Some(["minecraft:in_fire".to_owned()].as_slice()),
        );
    }

    #[test]
    fn a_dimension_type_missing_has_skylight_is_an_error_not_a_default() {
        // `has_skylight` is `fieldOf`, not `optionalFieldOf`, in vanilla's codec.
        // Defaulting it would be the same class of bug as the sky-light height gap, in a new place: an unparseable
        // entry must read as "unknown", so the adapter falls back, rather than
        // as "the overworld".
        let err = DimensionType::from_nbt(&Nbt::Compound(vec![(
            "has_ceiling".to_owned(),
            Nbt::Byte(0),
        )]))
        .expect_err("a missing required field must not default");
        assert!(err.to_string().contains("has_skylight"), "got {err}");
    }
}
