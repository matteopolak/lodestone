//! One saved entity record and its NBT codec: pure data, shared by the native
//! `entities/` region files ([`crate::entity_storage`]) and the in-memory paths
//! (a bee stored in a hive) that must also run in the browser.

use lodestone_core::{Nbt, NbtTag};
use lodestone_model::{ItemStack, ResourceKey, Rotation, Vec3};
use uuid::Uuid;

/// One persisted entity, in the subset this server actually simulates.
///
/// Deliberately **not** a full vanilla entity: the oracle files carry `Brain`,
/// `attributes`, `memories` and ~30 more fields per mob, none of which this
/// server models. Those are carried through verbatim in
/// [`extra`](Self::extra) for exactly the reason
/// [`crate::player_data::PlayerData::preserved`] exists — a writer that emitted
/// only what it understands would strip a real world's mobs down to a position
/// and a health value on the first save.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedEntity {
    /// The entity type key, e.g. `minecraft:cow` or `minecraft:item`.
    pub id: ResourceKey,
    /// Vanilla's `UUID` int-array, round-tripped — see the module doc on stale
    /// record clearing for why this must not be regenerated on load.
    pub uuid: Uuid,
    /// Feet position.
    pub pos: Vec3,
    /// Velocity, blocks per tick.
    pub motion: Vec3,
    /// Look direction.
    pub rotation: Rotation,
    /// Health, for a living entity.
    pub health: Option<f32>,
    /// For `minecraft:item` only: the stack it holds, components included.
    pub item: Option<ItemStack>,
    /// For `minecraft:item` only: ticks alive, vanilla's `Age`.
    pub age: Option<i16>,
    /// For `minecraft:item` only: vanilla's `PickupDelay`.
    pub pickup_delay: Option<i16>,
    /// Every other field the source record carried, verbatim.
    pub extra: Vec<(String, Nbt)>,
}

/// The fields [`SavedEntity::to_nbt`] **always** writes, and which are therefore
/// always excluded from [`SavedEntity::extra`].
///
/// Everything else is excluded only if it actually decoded — see
/// [`SavedEntity::from_nbt`]'s "consumed set" note, which is not a style choice
/// but a bug fix paid for by a real vanilla world.
const ALWAYS_WRITTEN: &[&str] = &["id", "UUID", "Pos", "Motion", "Rotation"];

impl SavedEntity {
    /// The chunk column this entity belongs to.
    ///
    /// `floor`, not truncation: `x = -1.5` is chunk `-1`, and truncating
    /// division would file every entity on the negative side of the origin one
    /// chunk too far in — the same arithmetic note
    /// [`crate::region_source`]'s `chunk_of` makes for block positions.
    #[must_use]
    pub fn chunk(&self) -> (i32, i32) {
        (
            (self.pos.x.floor() as i64).div_euclid(16) as i32,
            (self.pos.z.floor() as i64).div_euclid(16) as i32,
        )
    }

    /// Encodes to the compound a vanilla server writes into an `Entities` list.
    #[must_use]
    pub fn to_nbt(&self) -> Nbt {
        let mut fields = vec![
            ("id".to_owned(), Nbt::String(self.id.to_string())),
            ("UUID".to_owned(), Nbt::IntArray(uuid_to_ints(self.uuid))),
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
        ];
        if let Some(health) = self.health {
            fields.push(("Health".to_owned(), Nbt::Float(health)));
        }
        if let Some(stack) = &self.item {
            let persisted = crate::item_nbt::stack_to_nbt(stack);
            if !persisted.complete {
                tracing::warn!(item = %stack.item, "a dropped stack carries components with no saved form; they are left out");
            }
            fields.push(("Item".to_owned(), Nbt::Compound(persisted.fields)));
        }
        if let Some(age) = self.age {
            fields.push(("Age".to_owned(), Nbt::Short(age)));
        }
        if let Some(delay) = self.pickup_delay {
            fields.push(("PickupDelay".to_owned(), Nbt::Short(delay)));
        }
        fields.extend(self.extra.iter().cloned());
        Nbt::Compound(fields)
    }

    /// Decodes one entry of an `Entities` list, or `None` if it carries no
    /// usable `id`/`Pos`.
    ///
    /// A single unreadable entity is dropped rather than failing the chunk, on
    /// the same argument [`crate::chunk_nbt`]'s container reader makes: one bad
    /// record must not cost the other 2092.
    ///
    /// # The "consumed set", and the bug it exists to prevent
    ///
    /// [`extra`](Self::extra) is built from the fields this function **did not
    /// actually decode**, not from a static list of the ones it knows about. The
    /// difference is not cosmetic, and the static version shipped a real defect
    /// that `entity_nbt_vanilla_oracle.rs` caught on its first run:
    ///
    /// | field | on `minecraft:item` | on a mob |
    /// |---|---|---|
    /// | `Age` | `Short` — ticks alive | **`Int`** — breeding age, negative for a baby |
    /// | `Health` | **`Short`** — a constant 5 | `Float` — real health |
    ///
    /// The same NBT key means two different things with two different tag types
    /// depending on the entity's *class*. A static exclusion list containing
    /// `"Age"` therefore matched the sheep's `Int` field, failed to decode it
    /// (this code wants a `Short`), and **dropped it from the output**: every
    /// baby sheep in a loaded world would have silently become an adult, with a
    /// clean parse and no error. This is exactly the collision shape
    /// `CLAUDE.md`'s entity-metadata-index rule describes, in NBT rather than in
    /// metadata indices, and the guard has to be "did the decode succeed?"
    /// because *which* classes collide on a given key is not knowable from the
    /// key alone.
    #[must_use]
    pub fn from_nbt(nbt: &Nbt) -> Option<Self> {
        let Some(Nbt::String(id)) = field(nbt, "id") else {
            return None;
        };
        let id: ResourceKey = id.parse().ok()?;
        let pos = read_doubles(field(nbt, "Pos"))?;

        let mut consumed: Vec<&str> = ALWAYS_WRITTEN.to_vec();

        let health = match field(nbt, "Health") {
            Some(Nbt::Float(h)) => {
                consumed.push("Health");
                Some(*h)
            }
            // Anything else — notably an item entity's `Short` — is left for
            // `extra` to carry verbatim.
            _ => None,
        };
        // Only a dropped item's `Item` is the entity's own stack. On a thrown
        // potion or a trident the same key carries a full stack with
        // components, which the projectile restore reads from `extra`.
        let item_field = if id.path() == "item" { field(nbt, "Item") } else { None };
        let item = match item_field.and_then(crate::item_nbt::stack_from_nbt).map(|mut stack| {
            // An item entity's lifecycle counts in a byte.
            stack.count = stack.count.min(255);
            stack
        }) {
            Some(stack) => {
                consumed.push("Item");
                Some(stack)
            }
            None => None,
        };
        let age = match field(nbt, "Age") {
            Some(Nbt::Short(a)) => {
                consumed.push("Age");
                Some(*a)
            }
            _ => None,
        };
        let pickup_delay = match field(nbt, "PickupDelay") {
            Some(Nbt::Short(d)) => {
                consumed.push("PickupDelay");
                Some(*d)
            }
            _ => None,
        };

        let extra = match nbt {
            Nbt::Compound(fields) => fields
                .iter()
                .filter(|(name, _)| !consumed.contains(&name.as_str()))
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        Some(Self {
            id,
            uuid: read_uuid(field(nbt, "UUID")).unwrap_or_else(Uuid::new_v4),
            pos,
            motion: read_doubles(field(nbt, "Motion")).unwrap_or(Vec3::new(0.0, 0.0, 0.0)),
            rotation: read_rotation(field(nbt, "Rotation")).unwrap_or(Rotation::new(0.0, 0.0)),
            health,
            item,
            age,
            pickup_delay,
            extra,
        })
    }
}

pub(crate) fn field<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value),
        _ => None,
    }
}

pub(crate) fn doubles(v: Vec3) -> Nbt {
    Nbt::List {
        element_type: NbtTag::Double,
        elements: vec![Nbt::Double(v.x), Nbt::Double(v.y), Nbt::Double(v.z)],
    }
}

pub(crate) fn read_doubles(nbt: Option<&Nbt>) -> Option<Vec3> {
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

pub(crate) fn read_rotation(nbt: Option<&Nbt>) -> Option<Rotation> {
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

/// Vanilla's nbt utils's create uuid: the 128 bits as four big-endian `int`s, most
/// significant first. Not a string, and not two longs — a `.dat` written with
/// either is silently unreadable by the real game.
pub(crate) fn uuid_to_ints(uuid: Uuid) -> Vec<i32> {
    let (hi, lo) = uuid.as_u64_pair();
    vec![
        (hi >> 32) as i32,
        (hi & 0xFFFF_FFFF) as i32,
        (lo >> 32) as i32,
        (lo & 0xFFFF_FFFF) as i32,
    ]
}

pub(crate) fn read_uuid(nbt: Option<&Nbt>) -> Option<Uuid> {
    let Some(Nbt::IntArray(parts)) = nbt else {
        return None;
    };
    if parts.len() != 4 {
        return None;
    }
    let word = |i: usize| u64::from(parts[i] as u32);
    Some(Uuid::from_u64_pair(
        (word(0) << 32) | word(1),
        (word(2) << 32) | word(3),
    ))
}

