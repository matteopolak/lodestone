//! The `slot` wire type at this protocol — an item stack as a **component
//! map**.
//!
//! # Wire layout
//!
//! ```text
//! varint count        // 0 means the slot is empty and nothing follows
//! varint item id
//! varint added-component count
//! varint removed-component count
//! added:   [varint component type, payload shaped by that type] * added
//! removed: [varint component type] * removed
//! ```
//!
//! The count is a **varint** here; the 1.20.6 era writes a signed byte. The
//! two agree for counts `0..=127` and disagree above, which no vanilla stack
//! reaches — but a decoder carried between the two eras also disagrees about
//! how many *bytes* the field is for a negative value, and a negative count is
//! exactly what a malformed stream produces.
//!
//! # Why this needs a payload table and NBT did not
//!
//! NBT is self-delimiting: a decoder that does not understand a tag can still
//! skip it. **A component payload is not.** Its length is implied entirely by
//! its type, so a decoder that meets an unknown component id cannot skip it,
//! cannot find the next component, and cannot find the end of the packet
//! either. [`skip_component_payload`] is therefore a per-type payload table,
//! and a type it does not know is a **loud error** rather than a silently
//! truncated stack. Any other choice desynchronises the connection at the next
//! slot in the same array.
//!
//! # Where the component ids come from
//!
//! The numbering is the `minecraft:data_component_type` registry, and it comes
//! from this era's own jar registry report — 104 entries, ids 0 through 103,
//! committed at `tests/support/registries_1_21_11_jar.json`. `minecraft-data`
//! lists the same 104 names for this protocol, which is a second independent
//! source agreeing; `tests/components.rs` asserts the table here against the
//! committed jar dump rather than against that agreement.
//!
//! # What this table refuses, and why refusing is right
//!
//! 26 of the 104 payloads are nested records that reach a registry-entry
//! holder, a block predicate, an attribute record, a sound-event holder or a
//! resolvable profile — structures this crate has no model for. Each is
//! refused by name. What is *not* acceptable is a length guess, so there is no
//! default arm.
//!
//! # The raw-bytes tail
//!
//! Every payload this table decodes is kept as the **exact wire bytes** it
//! consumed alongside its type id, and encoding writes them back verbatim.
//! That keeps a stack lossless without needing an encoder for every component
//! shape.

use lodestone_core::{Ctx, Decode, Encode, Error, Reader, Result, Writer, read_network_nbt};

/// Upper bound on a component string payload, matching the vanilla limit.
const MAX_STRING: usize = 32_767;

/// One component on a stack: its registry id, and the exact bytes of its
/// payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotComponent {
    /// Component type id from `minecraft:data_component_type`.
    pub type_id: i32,
    /// The payload exactly as it arrived, empty for a component whose type
    /// carries none.
    pub payload: Vec<u8>,
}

/// A single inventory slot: either empty or a stack with its components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    /// An empty slot (`count == 0`).
    Empty,
    /// An occupied slot.
    Item {
        /// Flat item registry id (VarInt on the wire).
        id: i32,
        /// Stack size; never zero, since zero is the empty encoding.
        count: i32,
        /// Components this stack adds to its item's defaults, in wire order.
        components: Vec<SlotComponent>,
        /// Component types this stack removes from its item's defaults.
        removed: Vec<i32>,
    },
}

impl Slot {
    /// Whether this slot is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        matches!(self, Slot::Empty)
    }
}

impl Encode for Slot {
    fn encode(&self, w: &mut Writer, _ctx: Ctx) -> Result<()> {
        match self {
            Slot::Empty => w.var_i32(0),
            Slot::Item {
                id,
                count,
                components,
                removed,
            } => {
                w.var_i32(*count);
                w.var_i32(*id);
                w.var_i32(i32::try_from(components.len()).unwrap_or(i32::MAX));
                w.var_i32(i32::try_from(removed.len()).unwrap_or(i32::MAX));
                for component in components {
                    w.var_i32(component.type_id);
                    w.bytes(&component.payload);
                }
                for type_id in removed {
                    w.var_i32(*type_id);
                }
            }
        }
        Ok(())
    }
}

impl Decode for Slot {
    fn decode(r: &mut Reader<'_>, ctx: Ctx) -> Result<Self> {
        let count = r.var_i32()?;
        if count == 0 {
            return Ok(Slot::Empty);
        }
        if count < 0 {
            return Err(Error::NegativeLength(count));
        }
        let id = r.var_i32()?;
        let added = read_count(r)?;
        let removed_count = read_count(r)?;
        let mut components = Vec::with_capacity(added.min(64));
        for _ in 0..added {
            let type_id = r.var_i32()?;
            let payload = read_component_payload(r, type_id, ctx)?;
            components.push(SlotComponent { type_id, payload });
        }
        let mut removed = Vec::with_capacity(removed_count.min(64));
        for _ in 0..removed_count {
            removed.push(r.var_i32()?);
        }
        Ok(Slot::Item {
            id,
            count,
            components,
            removed,
        })
    }
}

/// Reads a VarInt count, rejecting a negative one rather than wrapping it.
fn read_count(r: &mut Reader<'_>) -> Result<usize> {
    let raw = r.var_i32()?;
    usize::try_from(raw).map_err(|_| Error::NegativeLength(raw))
}

/// Consumes one component payload and returns the exact bytes it occupied.
///
/// The span is sliced out of the reader's own buffer after the parse:
/// `remaining_bytes` is tied to the buffer lifetime rather than the borrow, so
/// the pre-read slice stays valid while the cursor advances.
fn read_component_payload(r: &mut Reader<'_>, type_id: i32, ctx: Ctx) -> Result<Vec<u8>> {
    let before = r.remaining_bytes();
    let start_len = before.len();
    skip_component_payload(r, type_id, ctx)?;
    let consumed = start_len - r.remaining_bytes().len();
    Ok(before[..consumed].to_vec())
}

/// Advances the reader past one component's payload, by type.
///
/// A type this function does not know is an error naming the id: see the
/// module docs for why a skip is impossible without the type.
fn skip_component_payload(r: &mut Reader<'_>, type_id: i32, ctx: Ctx) -> Result<()> {
    match type_id {
        // Components with no payload at all: `unbreakable`,
        // `creative_slot_lock`, `intangible_projectile`, `glider`.
        4 | 20 | 22 | 34 => {}
        // A single VarInt. The long tail from 79 up is the per-mob variant
        // and colour set, every one of them a plain registry ordinal.
        1..=3 | 12 | 19 | 31 | 44 | 46 | 61 | 71 | 79..=92 | 95 | 96 | 98..=103 => {
            r.var_i32()?;
        }
        // A single boolean: `enchantment_glint_override`.
        21 => {
            r.bool()?;
        }
        // A packed 32-bit colour: `dyed_color`, `map_color`.
        42 | 43 => {
            r.i32()?;
        }
        // A single float: `minimum_attack_charge`, `potion_duration_scale`.
        7 | 50 => {
            r.f32()?;
        }
        // A single string.
        10 | 27 | 35 | 63 | 69 => {
            r.string(MAX_STRING)?;
        }
        // Anonymous NBT, which may be a bare TAG_End.
        0 | 6 | 9 | 45 | 55 | 57 | 64 | 76 | 77 => {
            read_network_nbt(r)?;
        }
        // A nested stack: `use_remainder`.
        25 => {
            Slot::decode(r, ctx)?;
        }
        // A varint-counted list of anonymous NBT values (the lore lines).
        11 => {
            let count = read_count(r)?;
            for _ in 0..count {
                read_network_nbt(r)?;
            }
        }
        // A varint-counted list of varints: `pot_decorations`.
        72 => {
            let count = read_count(r)?;
            for _ in 0..count {
                r.var_i32()?;
            }
        }
        // A varint-counted list of nested stacks: `charged_projectiles`,
        // `bundle_contents`, `container`.
        47 | 48 | 73 => {
            let count = read_count(r)?;
            for _ in 0..count {
                Slot::decode(r, ctx)?;
            }
        }
        // A varint-counted list of `(varint, varint)` pairs, with **no**
        // trailing show-in-tooltip flag — that flag moved into the separate
        // `tooltip_display` component, so a decoder carried forward from the
        // 1.20.6 era reads one byte too many here. `enchantments`,
        // `stored_enchantments`, `suspicious_stew_effects`.
        13 | 41 | 51 => {
            let count = read_count(r)?;
            for _ in 0..count {
                r.var_i32()?;
                r.var_i32()?;
            }
        }
        // Block-state properties: `(name, value)` string pairs.
        74 => {
            let count = read_count(r)?;
            for _ in 0..count {
                r.string(MAX_STRING)?;
                r.string(MAX_STRING)?;
            }
        }
        // `use_effects`: two booleans and a speed multiplier.
        5 => {
            r.bool()?;
            r.bool()?;
            r.f32()?;
        }
        // `weapon`: item damage per attack, then a blocking-disable duration.
        29 => {
            r.var_i32()?;
            r.f32()?;
        }
        // `attack_range`: six floats.
        30 => {
            for _ in 0..6 {
                r.f32()?;
            }
        }
        // `swing_animation`: an animation ordinal and a duration.
        40 => {
            r.var_i32()?;
            r.var_i32()?;
        }
        // `use_cooldown`: seconds, then an optional cooldown-group id.
        26 => {
            r.f32()?;
            if r.bool()? {
                r.string(MAX_STRING)?;
            }
        }
        // `entity_data`, `block_entity_data`: a type ordinal and an NBT blob.
        56 | 58 => {
            r.var_i32()?;
            read_network_nbt(r)?;
        }
        // `lodestone_tracker`: an optional dimension-qualified position, then
        // a tracked flag.
        65 => {
            if r.bool()? {
                r.string(MAX_STRING)?;
                r.i64()?;
            }
            r.bool()?;
        }
        // `tooltip_display`: a hide-everything flag, then the component ids
        // whose own tooltip lines are hidden.
        18 => {
            r.bool()?;
            let count = read_count(r)?;
            for _ in 0..count {
                r.var_i32()?;
            }
        }
        // `custom_model_data`: four independent lists — floats, flags,
        // strings and packed colours.
        17 => {
            let floats = read_count(r)?;
            for _ in 0..floats {
                r.f32()?;
            }
            let flags = read_count(r)?;
            for _ in 0..flags {
                r.bool()?;
            }
            let strings = read_count(r)?;
            for _ in 0..strings {
                r.string(MAX_STRING)?;
            }
            let colors = read_count(r)?;
            for _ in 0..colors {
                r.i32()?;
            }
        }
        // Registry-entry holders whose inline form is a bare identifier:
        // `chicken/variant`, `zombie_nautilus/variant`.
        93 | 94 => {
            if r.var_i32()? == 0 {
                r.string(MAX_STRING)?;
            }
        }
        // `painting/variant`: a holder whose inline form is a whole variant
        // record, so only the registry-id form is accepted.
        97 => {
            super::common::read_registry_holder_id(r, "painting variant component")?;
        }
        // Everything else — see the module docs. Each is a nested structure
        // reaching a registry holder, a block predicate, an attribute record,
        // a sound-event holder or a resolvable profile that this crate has no
        // model for, and a payload whose length cannot be derived must not be
        // guessed at.
        other => {
            return Err(Error::InvalidEnumVariant {
                name: "1.21.11 item component payload",
                value: other,
            });
        }
    }
    Ok(())
}
