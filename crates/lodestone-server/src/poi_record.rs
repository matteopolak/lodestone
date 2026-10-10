//! Point-of-interest records (workstations, beds, bells) and their NBT codec:
//! pure data, shared by the native `poi/` region files ([`crate::poi_storage`])
//! and the in-memory villager claim ledgers, which must also run in the browser.

use std::collections::BTreeMap;

use lodestone_core::{Nbt, NbtTag};
use lodestone_model::{BlockPos, ResourceKey};

/// Maximum simultaneous claims by resource path. All built-in keys use the
/// `minecraft` namespace, so matching on [`ResourceKey::path`] avoids
/// allocating a full [`ResourceKey`] per arm.
///
/// An unrecognised type gets `0`, the same result as a block that is not a POI.
/// This conservative default prevents an unknown type from handing out claims
/// that the rest of the server cannot validate.
#[must_use]
pub fn max_tickets(poi_type: &ResourceKey) -> i32 {
    match poi_type.path() {
        "armorer" | "butcher" | "cartographer" | "cleric" | "farmer" | "fisherman"
        | "fletcher" | "leatherworker" | "librarian" | "mason" | "shepherd" | "toolsmith"
        | "weaponsmith" | "home" => 1,
        "meeting" => 32,
        _ => 0,
    }
}

/// One point-of-interest record — `PoiRecord`.
#[derive(Debug, Clone, PartialEq)]
pub struct PoiRecord {
    /// The block this POI is anchored to.
    pub pos: BlockPos,
    /// The POI type key, e.g. `minecraft:nether_portal`.
    pub poi_type: ResourceKey,
    /// Remaining simultaneous claims. See the module doc: **zero** is what an
    /// *absent* `free_tickets` field on disk means, not "unclaimed".
    pub free_tickets: i32,
}

impl PoiRecord {
    /// Creates a freshly discovered POI with its type's full ticket count.
    #[must_use]
    pub fn new(pos: BlockPos, poi_type: ResourceKey) -> Self {
        let free_tickets = max_tickets(&poi_type);
        Self {
            pos,
            poi_type,
            free_tickets,
        }
    }

    /// Returns whether at least one simultaneous claim remains.
    #[must_use]
    pub fn has_space(&self) -> bool {
        self.free_tickets > 0
    }

    /// Returns whether at least one ticket has been claimed.
    #[must_use]
    pub fn is_occupied(&self) -> bool {
        self.free_tickets != max_tickets(&self.poi_type)
    }

    /// Claims one ticket, returning `false` without mutation when none remain.
    pub fn acquire_ticket(&mut self) -> bool {
        if self.free_tickets <= 0 {
            return false;
        }
        self.free_tickets -= 1;
        true
    }

    /// Releases one ticket, returning `false` without mutation when already at
    /// the type's maximum.
    #[cfg(test)]
    pub fn release_ticket(&mut self) -> bool {
        if self.free_tickets >= max_tickets(&self.poi_type) {
            return false;
        }
        self.free_tickets += 1;
        true
    }

    /// Encodes the record fields used by the POI interchange format.
    pub(crate) fn to_nbt(&self) -> Nbt {
        let mut fields = vec![
            (
                "pos".to_owned(),
                Nbt::IntArray(vec![self.pos.x, self.pos.y, self.pos.z]),
            ),
            ("type".to_owned(), Nbt::String(self.poi_type.to_string())),
        ];
        // The optional field is omitted exactly at its default of zero. See
        // the module doc for why absence means no free tickets.
        if self.free_tickets != 0 {
            fields.push(("free_tickets".to_owned(), Nbt::Int(self.free_tickets)));
        }
        Nbt::Compound(fields)
    }

    /// Decodes the record fields. `None` if `pos` or `type` do not parse; a
    /// single bad record must not discard the rest of the section.
    pub(crate) fn from_nbt(nbt: &Nbt) -> Option<Self> {
        let pos = match field(nbt, "pos") {
            Some(Nbt::IntArray(parts)) if parts.len() == 3 => {
                BlockPos::new(parts[0], parts[1], parts[2])
            }
            _ => return None,
        };
        let poi_type: ResourceKey = match field(nbt, "type") {
            Some(Nbt::String(s)) => s.parse().ok()?,
            _ => return None,
        };
        let free_tickets = match field(nbt, "free_tickets") {
            Some(Nbt::Int(v)) => *v,
            _ => 0,
        };
        Some(Self {
            pos,
            poi_type,
            free_tickets,
        })
    }
}

/// One section's worth of POI records — `PoiSection`.
#[derive(Debug, Clone, Default)]
pub struct PoiSection {
    /// `PoiSection.isValid` — whether this section's records are believed to
    /// match the blocks currently there. Nothing in this codebase re-derives
    /// POI from a block scan yet (see the module doc's scope note), so a
    /// Sections built here are valid at construction; the field preserves the
    /// validity flag when an existing section is read and written again.
    pub valid: bool,
    /// Every record in the section, keyed by nothing but its own `pos` — see
    /// [`Self::add`] for how a collision at the same position is resolved.
    pub records: Vec<PoiRecord>,
}

impl PoiSection {
    /// An empty, valid section — `new PoiSection(setDirty)`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            valid: true,
            records: Vec::new(),
        }
    }

    /// Adds a POI at `pos`, inlined with the record insertion it performs.
    ///
    /// Three cases are distinguished: a new position inserts and returns
    /// `true`; the same type at the position is a no-op returning `false`; a
    /// different type replaces the existing record and returns `true`.
    pub fn add(&mut self, pos: BlockPos, poi_type: ResourceKey) -> bool {
        if let Some(idx) = self.records.iter().position(|r| r.pos == pos) {
            if self.records[idx].poi_type == poi_type {
                return false;
            }
            self.records[idx] = PoiRecord::new(pos, poi_type);
            return true;
        }
        self.records.push(PoiRecord::new(pos, poi_type));
        true
    }

    /// Inserts an already-built record verbatim, replacing whatever was at
    /// `record.pos`. Unlike [`Self::add`], this keeps `record`'s own
    /// `free_tickets` rather than resetting it to the type's full count —
    /// for a caller that already knows a record's exact state, such as a
    /// conversion from a live in-memory index
    /// ([`crate::portal::poi_records_for_index`]) or a reload from disk,
    /// as opposed to [`Self::add`]'s use when a block was *just* discovered
    /// and its ticket count starts full.
    ///
    /// Returns `true` if this was a new position, `false` if it replaced an
    /// existing record.
    pub fn insert_record(&mut self, record: PoiRecord) -> bool {
        if let Some(idx) = self.records.iter().position(|r| r.pos == record.pos) {
            self.records[idx] = record;
            return false;
        }
        self.records.push(record);
        true
    }

    /// Removes the record at `pos`, returning `true` when one was present.
    pub fn remove(&mut self, pos: BlockPos) -> bool {
        let before = self.records.len();
        self.records.retain(|r| r.pos != pos);
        self.records.len() != before
    }

    /// Returns the record at `pos`, read-only.
    #[must_use]
    pub fn get(&self, pos: BlockPos) -> Option<&PoiRecord> {
        self.records.iter().find(|r| r.pos == pos)
    }

    /// The same record, mutable — for [`PoiRecord::acquire_ticket`]/
    /// [`PoiRecord::release_ticket`] callers.
    pub fn get_mut(&mut self, pos: BlockPos) -> Option<&mut PoiRecord> {
        self.records.iter_mut().find(|r| r.pos == pos)
    }

    /// Returns records matching a type predicate and occupancy state. The
    /// section is small enough that a linear scan is the appropriate index.
    pub fn records_matching<'a>(
        &'a self,
        mut type_predicate: impl FnMut(&ResourceKey) -> bool + 'a,
        occupancy: Occupancy,
    ) -> impl Iterator<Item = &'a PoiRecord> + 'a {
        self.records
            .iter()
            .filter(move |r| type_predicate(&r.poi_type))
            .filter(move |r| occupancy.test(r))
    }

    pub(crate) fn to_nbt(&self) -> Nbt {
        Nbt::Compound(vec![
            ("Valid".to_owned(), Nbt::Byte(i8::from(self.valid))),
            (
                "Records".to_owned(),
                Nbt::List {
                    element_type: NbtTag::Compound,
                    elements: self.records.iter().map(PoiRecord::to_nbt).collect(),
                },
            ),
        ])
    }

    /// Reads the optional `Valid` flag; omission decodes to `false`, while
    /// [`Self::new`] creates an in-memory section with `valid = true`.
    pub(crate) fn from_nbt(nbt: &Nbt) -> Self {
        let valid = matches!(field(nbt, "Valid"), Some(Nbt::Byte(b)) if *b != 0);
        let records = match field(nbt, "Records") {
            Some(Nbt::List { elements, .. }) => {
                elements.iter().filter_map(PoiRecord::from_nbt).collect()
            }
            _ => Vec::new(),
        };
        Self { valid, records }
    }
}

/// Claim state a record query can require.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occupancy {
    /// At least one ticket remains available for a new claim.
    HasSpace,
    /// At least one ticket has been claimed.
    IsOccupied,
    /// No claim-state filter.
    Any,
}

impl Occupancy {
    fn test(self, record: &PoiRecord) -> bool {
        match self {
            Self::HasSpace => record.has_space(),
            Self::IsOccupied => record.is_occupied(),
            Self::Any => true,
        }
    }
}

/// Every section of one chunk column — the root of one `poi/` chunk NBT tree.
#[derive(Debug, Clone, Default)]
pub struct PoiChunk {
    /// Keyed by signed section Y (for example, `-4..=19` in the overworld).
    /// Sections with no POI are absent from the map.
    pub sections: BTreeMap<i32, PoiSection>,
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

