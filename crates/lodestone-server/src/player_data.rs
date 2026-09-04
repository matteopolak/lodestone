//! Per-player persistence: the schema half of `<world>/players/data/<uuid>.dat`.
//!
//! # What it is
//!
//! The thing that makes a player's inventory, position and health survive a
//! disconnect. Before this, `grep -rn 'playerdata\|PlayerDataStorage'` across
//! every `.rs` file in the workspace matched exactly one *comment*, in a shell
//! test: there was no per-uuid `.dat` code of any kind, so quitting a world threw
//! away everything the player was carrying, where they were standing, and how
//! hurt they were.
//!
//! [`lodestone_anvil::player_dat`] owns the container (path, gzip, the temp/
//! `.dat_old`/rename write). This module owns the *schema* — which NBT fields
//! mean what — for the same reason `chunk_nbt` is separate from
//! `lodestone_anvil::region`: the container is version-free and reusable, the
//! schema is 26.2's.
//!
//! # Every field name here came out of a real vanilla file
//!
//! Not from a wiki page and not from `decode(encode(x))`. The oracle world at
//! `.cache/mc/survival/world` has 287 player files written by a real 26.2
//! server, and they were read with a foreign parser (Python `gzip` +
//! `struct.unpack`, sharing no code with this repo) to get the exact names,
//! tag types and nesting used below. The ones that surprise:
//!
//! | field | tag | note |
//! |---|---|---|
//! | `Pos` | `List<Double>` × 3 | not three scalars |
//! | `Rotation` | `List<Float>` × 2 | yaw then pitch |
//! | `Air` | `Short` | not `Int` |
//! | `Fire` | `Short` | negative when not burning |
//! | `fall_distance` | `Double` | snake_case, unlike its neighbours |
//! | `Inventory` | `List<Compound>` | `{Slot: Byte, id: String, count: Int, components: Compound?}` |
//! | `playerGameType` | `Int` | camelCase, and *not* `GameType` |
//! | `SelectedItemSlot` | `Int` | the hotbar index |
//! | `XpLevel` | `Int` | the level, **not** the lifetime total |
//! | `XpP` | `Float` | the bar fraction, `0.0..1.0` |
//! | `XpTotal` | `Int` | the lifetime total |
//!
//! The three `Xp*` fields are three independent numbers and two of them are
//! adjacent `Int`s, so a transposition of `XpLevel` and `XpTotal` reads back as a
//! legal file with a wildly wrong level — the NBT twin of the wire transposition
//! [`crate::experience`] warns about. `XpSeed` is vanilla's enchanting-table roll
//! seed and is deliberately *not* modelled: it is carried through
//! [`preserved`](PlayerData::preserved) untouched.
//!
//! Note `count` is lowercase and an `Int`: that is 26.2's item form, not the
//! pre-1.20.5 `Count: Byte`. The same shape [`crate::chunk_nbt`] already writes
//! for container slots, which is why this module reuses that reading.
//!
//! # Unmodelled fields are carried through, not dropped
//!
//! [`PlayerData`] keeps every root field it does not understand in
//! [`PlayerData::preserved`] and writes it back untouched. This is the single
//! most important property in the module and it is not an optimisation: this
//! server models no hunger, no ender chest, no advancement tree
//! inside the `.dat`, and no `Brain`. A writer that emitted only what it
//! understands would **delete** all of that from a real player's file on the
//! first save — the same class of defect as re-saving a world and erasing its
//! cave biomes. Preserve first, model later.
//!
//! # How to change it, and the gotchas
//!
//! - **Adding a modelled field means removing it from the preserved set**, or
//!   the writer emits it twice. [`PlayerData::from_nbt`] filters by
//!   [`MODELLED_FIELDS`]; add the name there and the filter follows.
//! - **`DataVersion` is checked on read** through
//!   [`lodestone_anvil::require_supported_data_version`], so a file
//!   from another game version is refused rather than mis-decoded. It is not in
//!   the preserved set, because [`to_nbt`](PlayerData::to_nbt) always stamps our
//!   own.
//! - **Health `0.0` is a dead player**, and a dead player is held on the death
//!   screen, which sends no chunks. Restoring a stored `0.0` therefore looks
//!   exactly like a total chunk blackout with a working join and working
//!   keep-alives. [`PlayerData::spawn_state`] is the accessor that exists to make
//!   that decision explicit at the call site rather than implicit in a field read.
//!
//! # Dependencies
//!
//! [`lodestone_anvil::player_dat`] for the container, `lodestone-core` for NBT,
//! and [`crate::inventory::PlayerInventory`] as the in-memory shape.

use std::path::{Path, PathBuf};

use lodestone_core::{Nbt, NbtTag, Reader, Writer, read_network_nbt, write_network_nbt};
use lodestone_model::{GameMode, ItemStack, Rotation, Vec3};

use crate::inventory::{PLAYER_NATIVE_SIZE, PlayerInventory};

/// Every root field [`PlayerData`] models itself, and therefore the ones
/// [`PlayerData::from_nbt`] must **not** copy into
/// [`preserved`](PlayerData::preserved).
///
/// Kept as one list rather than repeated string literals in the reader and the
/// writer, because the failure mode of the two drifting apart is a duplicated NBT
/// key — legal on the wire, and read back as whichever copy the parser hits last.
const MODELLED_FIELDS: &[&str] = &[
    "DataVersion",
    "Pos",
    "Motion",
    "Rotation",
    "Health",
    "Air",
    "Fire",
    "fall_distance",
    "OnGround",
    "Dimension",
    "playerGameType",
    "SelectedItemSlot",
    "Inventory",
    "XpLevel",
    "XpP",
    "XpTotal",
];

/// A vanilla-shaped player save, as this server models it.
///
/// See the module doc for provenance and for why [`preserved`](Self::preserved)
/// exists.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerData {
    /// Feet position.
    pub pos: Vec3,
    /// Velocity, blocks per tick.
    pub motion: Vec3,
    /// Look direction.
    pub rotation: Rotation,
    /// Health, `0.0` for a dead player — see the module doc's warning.
    pub health: f32,
    /// Air supply in ticks, vanilla's `Air`.
    pub air_supply: i32,
    /// Burning ticks, vanilla's `Fire` (negative when not burning).
    pub fire: i16,
    /// Accumulated fall distance.
    pub fall_distance: f64,
    /// Whether the player was standing on something.
    pub on_ground: bool,
    /// The dimension key the player was in.
    pub dimension: String,
    /// The game mode, or `None` if the file had no readable `playerGameType`.
    pub game_mode: Option<GameMode>,
    /// The selected hotbar index.
    pub selected_slot: u8,
    /// The native-order inventory slots, exactly [`PLAYER_NATIVE_SIZE`] long.
    pub inventory: Vec<Option<ItemStack>>,
    /// The level, bar and lifetime total — vanilla's `XpLevel` / `XpP` /
    /// `XpTotal`.
    ///
    /// Held as the live [`crate::experience::PlayerExperience`] rather than three
    /// loose numbers so the clamping in
    /// [`PlayerExperience::restored`](crate::experience::PlayerExperience::restored)
    /// happens once, at decode, instead of once per caller. Before this was
    /// modelled the three fields rode through [`preserved`](Self::preserved), so a
    /// player's XP survived the *file* and not the *session*: it was written back
    /// verbatim on every save and never read into the running player, who joined at
    /// zero.
    pub experience: crate::experience::PlayerExperience,
    /// Every root field this module does not model, verbatim, written back
    /// unchanged. See the module doc — this is what stops a save deleting the
    /// player's experience, hunger and ender chest.
    pub preserved: Vec<(String, Nbt)>,
}

impl Default for PlayerData {
    fn default() -> Self {
        Self {
            pos: Vec3::new(0.0, 0.0, 0.0),
            motion: Vec3::new(0.0, 0.0, 0.0),
            rotation: Rotation::new(0.0, 0.0),
            health: 20.0,
            air_supply: 300,
            fire: -20,
            fall_distance: 0.0,
            on_ground: true,
            dimension: "minecraft:overworld".to_owned(),
            game_mode: None,
            selected_slot: 0,
            inventory: vec![None; PLAYER_NATIVE_SIZE],
            experience: crate::experience::PlayerExperience::default(),
            preserved: Vec::new(),
        }
    }
}

/// What a restored player should join as: where, and whether they are alive.
///
/// A struct rather than a tuple because the *dead* case is the trap the module
/// doc describes, and a bare `bool` at a call site reads as noise.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpawnState {
    /// Where to place the player.
    pub pos: Vec3,
    /// Which way they are facing.
    pub rotation: Rotation,
    /// `false` when the stored health was `<= 0.0`.
    pub alive: bool,
}

impl PlayerData {
    /// Captures the live state a connection holds.
    ///
    /// `preserved` starts from whatever was loaded at join (pass the loaded
    /// value's own `preserved`), so a field this server does not model survives a
    /// full load/modify/save cycle rather than only surviving until the first
    /// save.
    ///
    /// `dimension` is the dimension `pos` is expressed in — **load-bearing**,
    /// not cosmetic: before this parameter existed, every save wrote the
    /// `Default` impl's hardcoded `"minecraft:overworld"` regardless of where
    /// the player actually was, so a player who died or disconnected in the
    /// Nether had their Nether-relative `pos` persisted under a dimension tag
    /// that claimed it was an overworld coordinate. The next join placed them
    /// at that raw position *in the overworld* — a semi-arbitrary point that
    /// is rarely at overworld surface height, which is the "I respawn buried
    /// in the ground" report this fixes. See the join-time consumer in
    /// `crate::server` for the other half: reading this field back and
    /// declining to trust a non-overworld position as an overworld one.
    #[must_use]
    pub fn capture(
        pos: Vec3,
        rotation: Rotation,
        health: f32,
        air_supply: i32,
        game_mode: GameMode,
        inventory: &PlayerInventory,
        experience: crate::experience::PlayerExperience,
        preserved: Vec<(String, Nbt)>,
        dimension: crate::dimension::Dimension,
    ) -> Self {
        Self {
            pos,
            rotation,
            health,
            air_supply,
            experience,
            game_mode: Some(game_mode),
            selected_slot: inventory.selected_hotbar_slot(),
            inventory: (0..PLAYER_NATIVE_SIZE)
                .map(|slot| inventory.native(slot).cloned())
                .collect(),
            preserved,
            dimension: dimension.key().to_owned(),
            ..Self::default()
        }
    }

    /// Where and how this player should re-enter the world.
    #[must_use]
    pub fn spawn_state(&self) -> SpawnState {
        SpawnState {
            pos: self.pos,
            rotation: self.rotation,
            alive: self.health > 0.0,
        }
    }

    /// Rebuilds a [`PlayerInventory`] from the stored slots.
    #[must_use]
    pub fn to_inventory(&self) -> PlayerInventory {
        let mut inventory = PlayerInventory::new();
        for (slot, stack) in self.inventory.iter().enumerate() {
            inventory.set_native(slot, stack.clone());
        }
        inventory.set_selected_hotbar_slot(self.selected_slot);
        inventory
    }

    /// Encodes to the root compound a vanilla server would write.
    ///
    /// Modelled fields first, then everything preserved — so a field that
    /// somehow appears in both lists is overridden by our own value rather than
    /// by whichever the reader reached last.
    ///
    /// # Errors
    ///
    /// [`lodestone_anvil::Error::Nbt`] when an inventory item's custom-data
    /// bytes are not one complete compound-shaped network-NBT value.
    #[must_use]
    pub fn to_nbt(&self) -> Result<Nbt, lodestone_anvil::Error> {
        let mut fields = vec![
            (
                "DataVersion".to_owned(),
                Nbt::Int(lodestone_anvil::level_dat::DATA_VERSION_26_2),
            ),
            ("Pos".to_owned(), doubles(self.pos)),
            ("Motion".to_owned(), doubles(self.motion)),
            (
                "Rotation".to_owned(),
                Nbt::List {
                    element_type: NbtTag::Float,
                    elements: vec![
                        Nbt::Float(self.rotation.yaw),
                        Nbt::Float(self.rotation.pitch),
                    ],
                },
            ),
            ("Health".to_owned(), Nbt::Float(self.health)),
            (
                "Air".to_owned(),
                Nbt::Short(i16::try_from(self.air_supply).unwrap_or(i16::MAX)),
            ),
            ("Fire".to_owned(), Nbt::Short(self.fire)),
            ("fall_distance".to_owned(), Nbt::Double(self.fall_distance)),
            ("OnGround".to_owned(), Nbt::Byte(i8::from(self.on_ground))),
            ("Dimension".to_owned(), Nbt::String(self.dimension.clone())),
            (
                "SelectedItemSlot".to_owned(),
                Nbt::Int(i32::from(self.selected_slot)),
            ),
            (
                "Inventory".to_owned(),
                inventory_to_nbt(&self.inventory)?,
            ),
            // Written in vanilla's own declaration order, and the *types* are the
            // part worth checking rather than the order: `XpLevel` and `XpTotal`
            // are both `Int` and `XpP` is a `Float`, so a level written into
            // `XpTotal` produces a file every parser accepts.
            ("XpLevel".to_owned(), Nbt::Int(self.experience.level())),
            ("XpP".to_owned(), Nbt::Float(self.experience.progress())),
            ("XpTotal".to_owned(), Nbt::Int(self.experience.total())),
        ];
        if let Some(mode) = self.game_mode {
            fields.push(("playerGameType".to_owned(), Nbt::Int(game_type_value(mode))));
        }
        fields.extend(self.preserved.iter().cloned());
        Ok(Nbt::Compound(fields))
    }

    /// Decodes a player root compound.
    ///
    /// Every field is optional and falls back to [`Default`]: a real file always
    /// carries all of them, but a *partial* file must produce a usable player
    /// rather than an error, because the alternative is refusing the join of
    /// someone whose save is merely old-shaped. The one thing that **is** refused
    /// is a `DataVersion` this build cannot read.
    ///
    /// # Errors
    ///
    /// [`lodestone_anvil::Error::UnsupportedDataVersion`] when the file was
    /// written by a different game version.
    pub fn from_nbt(nbt: &Nbt) -> Result<Self, lodestone_anvil::Error> {
        lodestone_anvil::require_supported_data_version(int_field(nbt, "DataVersion"))?;
        let mut data = Self {
            pos: read_doubles(field(nbt, "Pos")).unwrap_or(Self::default().pos),
            motion: read_doubles(field(nbt, "Motion")).unwrap_or(Vec3::new(0.0, 0.0, 0.0)),
            rotation: read_rotation(field(nbt, "Rotation")).unwrap_or(Rotation::new(0.0, 0.0)),
            health: match field(nbt, "Health") {
                Some(Nbt::Float(h)) => *h,
                _ => 20.0,
            },
            air_supply: match field(nbt, "Air") {
                Some(Nbt::Short(a)) => i32::from(*a),
                _ => 300,
            },
            fire: match field(nbt, "Fire") {
                Some(Nbt::Short(f)) => *f,
                _ => -20,
            },
            fall_distance: match field(nbt, "fall_distance") {
                Some(Nbt::Double(d)) => *d,
                _ => 0.0,
            },
            on_ground: matches!(field(nbt, "OnGround"), Some(Nbt::Byte(b)) if *b != 0),
            dimension: match field(nbt, "Dimension") {
                Some(Nbt::String(s)) => s.clone(),
                _ => "minecraft:overworld".to_owned(),
            },
            game_mode: int_field(nbt, "playerGameType").and_then(game_type_from_value),
            selected_slot: int_field(nbt, "SelectedItemSlot")
                .and_then(|s| u8::try_from(s).ok())
                .unwrap_or(0),
            inventory: inventory_from_nbt(field(nbt, "Inventory")),
            // Each field falls back to its own zero rather than the triple falling
            // back together: a file with `XpLevel` and no `XpP` is a partial file,
            // and rejecting the level because the bar is missing loses more than it
            // protects. `restored` then clamps the result.
            experience: crate::experience::PlayerExperience::restored(
                int_field(nbt, "XpLevel").unwrap_or(0),
                match field(nbt, "XpP") {
                    Some(Nbt::Float(p)) => *p,
                    _ => 0.0,
                },
                int_field(nbt, "XpTotal").unwrap_or(0),
            ),
            preserved: Vec::new(),
        };
        if let Nbt::Compound(fields) = nbt {
            data.preserved = fields
                .iter()
                .filter(|(name, _)| !MODELLED_FIELDS.contains(&name.as_str()))
                .cloned()
                .collect();
        }
        Ok(data)
    }
}

/// Reads and writes player `.dat` files under one world directory.
///
/// # Why this holds no lock
///
/// It is a path and nothing else — every operation goes straight to the
/// filesystem, so two clones are interchangeable and there is no shared mutable
/// state to migrate when the server moves to an ECS proposal queue. Cloning is
/// an `Arc` bump of the directory path.
///
/// One player's file is only ever touched by that player's own connection task,
/// so there is no cross-connection race to guard: offline mode derives the uuid
/// from the username, which means two connections with the *same name* would
/// share a file — and vanilla refuses the second such login before it reaches
/// here.
#[derive(Debug, Clone)]
pub struct PlayerDataStore {
    dir: std::sync::Arc<PathBuf>,
}

impl PlayerDataStore {
    /// Roots a store at `world_dir`, creating `players/data` eagerly.
    ///
    /// Eager creation for the reason [`crate::region_source::RegionChunkSource::new`]
    /// gives: a later save must not fail for a reason the caller could have been
    /// told about at world open.
    ///
    /// # Errors
    ///
    /// [`lodestone_anvil::Error::Io`] if the directory cannot be created.
    pub fn new(world_dir: &Path) -> Result<Self, lodestone_anvil::Error> {
        let dir = lodestone_anvil::player_dat::dir_in(world_dir);
        std::fs::create_dir_all(&dir).map_err(lodestone_anvil::Error::Io)?;
        Ok(Self {
            dir: std::sync::Arc::new(dir),
        })
    }

    /// The file `uuid`'s data lives in.
    #[must_use]
    pub fn path_for(&self, uuid: uuid::Uuid) -> PathBuf {
        self.dir.join(format!("{uuid}.dat"))
    }

    /// Loads `uuid`'s saved state, or `Ok(None)` for a player who has never
    /// saved.
    ///
    /// # Errors
    ///
    /// The file exists but will not decode, or was written by another game
    /// version. Both are refusals rather than a silent empty
    /// player: overwriting a save we cannot read is the one outcome that loses
    /// data irrecoverably.
    pub fn read(&self, uuid: uuid::Uuid) -> Result<Option<PlayerData>, lodestone_anvil::Error> {
        let Some(root) = lodestone_anvil::player_dat::read_from_file(&self.path_for(uuid))? else {
            return Ok(None);
        };
        PlayerData::from_nbt(&root).map(Some)
    }

    /// Writes `data` as `uuid`'s saved state.
    ///
    /// # Errors
    ///
    /// [`lodestone_anvil::Error::Io`] on a filesystem failure, or
    /// [`lodestone_anvil::Error::Nbt`] if the tree will not encode.
    pub fn write(
        &self,
        uuid: uuid::Uuid,
        data: &PlayerData,
    ) -> Result<(), lodestone_anvil::Error> {
        let root = data.to_nbt()?;
        lodestone_anvil::player_dat::write_to_file(&root, &self.path_for(uuid))
    }
}

fn field<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value),
        _ => None,
    }
}

fn int_field(nbt: &Nbt, key: &str) -> Option<i32> {
    match field(nbt, key) {
        Some(Nbt::Int(v)) => Some(*v),
        _ => None,
    }
}

fn doubles(v: Vec3) -> Nbt {
    Nbt::List {
        element_type: NbtTag::Double,
        elements: vec![Nbt::Double(v.x), Nbt::Double(v.y), Nbt::Double(v.z)],
    }
}

fn read_doubles(nbt: Option<&Nbt>) -> Option<Vec3> {
    let Some(Nbt::List { elements, .. }) = nbt else {
        return None;
    };
    if elements.len() < 3 {
        return None;
    }
    let get = |i: usize| match elements[i] {
        Nbt::Double(d) => Some(d),
        _ => None,
    };
    Some(Vec3::new(get(0)?, get(1)?, get(2)?))
}

fn read_rotation(nbt: Option<&Nbt>) -> Option<Rotation> {
    let Some(Nbt::List { elements, .. }) = nbt else {
        return None;
    };
    if elements.len() < 2 {
        return None;
    }
    let get = |i: usize| match elements[i] {
        Nbt::Float(f) => Some(f),
        _ => None,
    };
    Some(Rotation::new(get(0)?, get(1)?))
}

/// Vanilla's own game-type ordinals: survival 0, creative 1,
/// adventure 2, spectator 3. Written as an explicit match rather than an `as`
/// cast off our own enum's declaration order, because that order is ours to
/// change and this number is not.
fn game_type_value(mode: GameMode) -> i32 {
    match mode {
        GameMode::Survival => 0,
        GameMode::Creative => 1,
        GameMode::Adventure => 2,
        GameMode::Spectator => 3,
    }
}

fn game_type_from_value(value: i32) -> Option<GameMode> {
    match value {
        0 => Some(GameMode::Survival),
        1 => Some(GameMode::Creative),
        2 => Some(GameMode::Adventure),
        3 => Some(GameMode::Spectator),
        _ => None,
    }
}

/// The `Inventory` list: `{Slot, id, count, components?}` per occupied slot,
/// empties omitted — the sparse persistent form (a real file with 12 items has
/// 12 entries, not 41). The only component currently converted is the
/// top-level `minecraft:custom_data` compound; its model bytes are validated
/// as complete network NBT before this function returns.
fn inventory_to_nbt(slots: &[Option<ItemStack>]) -> Result<Nbt, lodestone_anvil::Error> {
    let elements = slots
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            let stack = slot.as_ref()?;
            Some(item_to_nbt(index, stack))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Nbt::List {
        element_type: NbtTag::Compound,
        elements,
    })
}

fn item_to_nbt(index: usize, stack: &ItemStack) -> Result<Nbt, lodestone_anvil::Error> {
    let mut fields = vec![
        ("Slot".to_owned(), Nbt::Byte(index as i8)),
        ("id".to_owned(), Nbt::String(stack.item.to_string())),
        (
            "count".to_owned(),
            Nbt::Int(i32::try_from(stack.count).unwrap_or(i32::MAX)),
        ),
    ];
    if let Some(bytes) = &stack.components.custom_data {
        fields.push((
            "components".to_owned(),
            Nbt::Compound(vec![(
                "minecraft:custom_data".to_owned(),
                custom_data_to_persistent(index, bytes)?,
            )]),
        ));
    }
    Ok(Nbt::Compound(fields))
}

fn custom_data_to_persistent(
    slot: usize,
    bytes: &[u8],
) -> Result<Nbt, lodestone_anvil::Error> {
    let mut reader = Reader::new(bytes);
    let value = read_network_nbt(&mut reader).map_err(lodestone_anvil::Error::Nbt)?;
    reader
        .ensure_empty()
        .map_err(lodestone_anvil::Error::Nbt)?;
    if !matches!(value, Nbt::Compound(_)) {
        return Err(lodestone_anvil::Error::Nbt(lodestone_core::Error::Custom(
            format!("inventory slot {slot} custom_data must have a compound root"),
        )));
    }
    Ok(value)
}

fn custom_data_to_network(value: &Nbt) -> Option<Vec<u8>> {
    let Nbt::Compound(_) = value else {
        return None;
    };
    let mut writer = Writer::default();
    write_network_nbt(&mut writer, value).ok()?;
    Some(writer.into_vec())
}

fn inventory_from_nbt(nbt: Option<&Nbt>) -> Vec<Option<ItemStack>> {
    let mut out = vec![None; PLAYER_NATIVE_SIZE];
    let Some(Nbt::List { elements, .. }) = nbt else {
        return out;
    };
    for entry in elements {
        // `Slot` is a `Byte` in every real file. Read as either, because a
        // dropped stack is worse than a tolerant parser.
        let slot = match field(entry, "Slot") {
            Some(Nbt::Byte(b)) => i32::from(*b),
            Some(Nbt::Int(i)) => *i,
            _ => continue,
        };
        let Ok(slot) = usize::try_from(slot) else {
            continue;
        };
        if slot >= PLAYER_NATIVE_SIZE {
            continue;
        }
        let Some(Nbt::String(id)) = field(entry, "id") else {
            continue;
        };
        let Ok(key) = id.parse() else {
            continue;
        };
        let count = match field(entry, "count") {
            Some(Nbt::Int(c)) => (*c).max(0) as u32,
            Some(Nbt::Byte(c)) => i32::from(*c).max(0) as u32,
            _ => 1,
        };
        let mut stack = ItemStack::new(key, count);
        if let Some(components) = field(entry, "components") {
            if let Some(custom_data) = field(components, "minecraft:custom_data")
                .and_then(custom_data_to_network)
            {
                stack.components.custom_data = Some(custom_data);
            }
        }
        out[slot] = Some(stack);
    }
    out
}
