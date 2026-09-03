//! The `slot` wire type at this protocol — an item stack as a **component
//! map**, which is the change that defines this era as much as the
//! configuration phase does.
//!
//! # What replaced what
//!
//! Every era below carries an item as `(id, count, damage/metadata, optional
//! NBT)`: one opaque compound holding everything the stack knows. Here the
//! NBT is gone and the stack carries a *typed, ordered list of components*:
//!
//! ```text
//! i8 count            // 0 means the slot is empty and nothing follows
//! varint item id
//! varint added-component count
//! varint removed-component count
//! added:   [varint component type, payload shaped by that type] * added
//! removed: [varint component type] * removed
//! ```
//!
//! # Why this needs a payload table and NBT did not
//!
//! NBT is self-delimiting: a decoder that does not understand a tag can still
//! skip it. **A component payload is not.** Its length is implied entirely by
//! its type, so a decoder that meets an unknown component id cannot skip it,
//! cannot find the next component, and cannot find the end of the packet
//! either. That is why [`ComponentValue`] exists: it is a per-type payload
//! table, and a type it does not know is a **loud error** rather than a
//! silently truncated stack. Any other choice desynchronises the connection
//! at the next slot in the same array.
//!
//! # Where the component ids come from
//!
//! The numbering is the `minecraft:data_component_type` registry, and it
//! comes from this era's own jar registry report — 56 entries, ids 0 through
//! 55, committed at `tests/support/registries_1_20_6_jar.json`.
//! `minecraft-data` lists the same 56 names at the same 56 ids, which is a
//! second, independent source agreeing; `tests/components.rs` asserts the
//! table here against the committed jar dump rather than against that
//! agreement.
//!
//! # The three payloads this table deliberately refuses
//!
//! * `intangible_projectile` — `minecraft-data` models it as an NBT value,
//!   while every other flag-shaped component of this era carries no payload
//!   at all. The two readings differ by one byte on the wire and no dump in
//!   this tree settles which is right, so it errors by name rather than
//!   guessing. Settling it needs a stack that actually carries the component,
//!   which is an oracle we do not have.
//! * `attribute_modifiers`, `food`, `tool`, `trim`, `instrument`, `profile`,
//!   `written_book_content`, `writable_book_content`, `potion_contents`,
//!   `firework_explosion`, `fireworks`, `banner_patterns`, `bees`,
//!   `suspicious_stew_effects`, `map_decorations`, `can_place_on`,
//!   `can_break`, `charged_projectiles`, `bundle_contents`, `container`,
//!   `pot_decorations`, `lodestone_tracker`, `dyed_color`, `block_state`,
//!   `recipes`, `debug_stick_state`, `enchantments`, `stored_enchantments` —
//!   each is a nested structure, and several nest a whole `Slot` inside
//!   themselves. They are decoded where the nesting is plain and refused by
//!   name where it reaches a registry-holder or predicate type this crate has
//!   no model for. What is *not* acceptable is a length guess, so each
//!   refusal is explicit.
//!
//! # The raw-bytes tail
//!
//! A stack must round-trip losslessly: the client echoes items back to the
//! server on a window click. Every payload this table decodes is therefore
//! kept as the **exact wire bytes** it consumed alongside its type id, and
//! encoding writes them back verbatim. That keeps the echo byte-exact without
//! needing an encoder for every component shape.

use lodestone_core::{Ctx, Decode, Encode, Error, Reader, Result, Writer, read_network_nbt};

/// Upper bound on a component string payload, matching the vanilla limit.
const MAX_STRING: usize = 32_767;

/// One component on a stack: its registry id, and the exact bytes of its
/// payload.
///
/// The bytes are retained rather than a parsed value because the only
/// consumers so far are "how many components does this stack carry" and "echo
/// it back unchanged". A component with a typed consumer gets a typed accessor
/// beside this, reading out of `payload`; growing a parsed variant per
/// component without a consumer would be sixty types of dead code.
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
        count: i8,
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
            Slot::Empty => w.i8(0),
            Slot::Item {
                id,
                count,
                components,
                removed,
            } => {
                w.i8(*count);
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

/// Maximum item nesting this decoder will walk.
///
/// Three component payloads at this protocol hold whole nested stacks, and a
/// nested stack declares its own component list, so the wire structure recurses
/// once per level with nothing bounding it: there is no length prefix and no
/// declared level count anywhere in the chain, so the depth is the sender's
/// choice. Unbounded, one crafted stack from any server a player joins
/// exhausts the decoding thread's stack and aborts the process.
///
/// The number is the deepest nesting the game itself will construct: 16 wraps
/// of bundle-in-bundle (a nested bundle costs a flat 1/16 of a bundle's weight
/// budget of 1, so the chain stops at 17 stacks), plus one container-item level
/// that can hold such a chain but not another container item, plus one level
/// for a stack named by a prototype component enclosing the whole thing. A
/// payload deeper than that is one no server following the game's own rules can
/// produce, and refusing it costs a packet.
const MAX_ITEM_NESTING: u32 = 16 + 1 + 1 + 1;

impl Decode for Slot {
    fn decode(r: &mut Reader<'_>, ctx: Ctx) -> Result<Self> {
        decode_nested(r, ctx, 0)
    }
}

/// [`Slot`]'s decode with the nesting level threaded through it.
///
/// The bound is checked here, at the one point every cycle through this
/// module's readers passes: a nested stack is only ever reached by
/// [`skip_component_payload`] calling back into this function, so a component
/// payload added to that table inherits the bound without its author having to
/// remember anything.
fn decode_nested(r: &mut Reader<'_>, ctx: Ctx, depth: u32) -> Result<Slot> {
    if depth >= MAX_ITEM_NESTING {
        return Err(Error::Custom(format!(
            "item stack nests deeper than {MAX_ITEM_NESTING} levels, \
             past the depth any stack the game constructs can reach"
        )));
    }
    let count = r.i8()?;
    if count == 0 {
        return Ok(Slot::Empty);
    }
    let id = r.var_i32()?;
    let added = read_count(r)?;
    let removed_count = read_count(r)?;
    let mut components = Vec::with_capacity(added.min(64));
    for _ in 0..added {
        let type_id = r.var_i32()?;
        let payload = read_component_payload(r, type_id, ctx, depth)?;
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

/// Reads a VarInt count, rejecting a negative one rather than wrapping it.
fn read_count(r: &mut Reader<'_>) -> Result<usize> {
    let raw = r.var_i32()?;
    usize::try_from(raw).map_err(|_| Error::NegativeLength(raw))
}

/// Consumes one component payload and returns the exact bytes it occupied.
///
/// The span is sliced out of the reader's own buffer after the parse, the same
/// technique the pre-component eras use for a slot's raw NBT:
/// `remaining_bytes` is tied to the buffer lifetime rather than the borrow, so
/// the pre-read slice stays valid while the cursor advances.
fn read_component_payload(
    r: &mut Reader<'_>,
    type_id: i32,
    ctx: Ctx,
    depth: u32,
) -> Result<Vec<u8>> {
    let before = r.remaining_bytes();
    let start_len = before.len();
    skip_component_payload(r, type_id, ctx, depth)?;
    let consumed = start_len - r.remaining_bytes().len();
    Ok(before[..consumed].to_vec())
}

/// Advances the reader past one component's payload, by type.
///
/// A type this function does not know is an error naming the id: see the
/// module docs for why a skip is impossible without the type.
fn skip_component_payload(r: &mut Reader<'_>, type_id: i32, ctx: Ctx, depth: u32) -> Result<()> {
    match type_id {
        // Components with no payload at all.
        14 | 15 | 17 | 21 => {}
        // A single VarInt.
        1 | 2 | 3 | 8 | 13 | 16 | 26 | 28 | 41 | 49 => {
            r.var_i32()?;
        }
        // A single boolean.
        4 | 18 => {
            r.bool()?;
        }
        // A fixed 32-bit integer.
        25 => {
            r.i32()?;
        }
        // Anonymous NBT, which may be a bare TAG_End.
        0 | 5 | 6 | 27 | 36 | 37 | 38 | 39 | 42 | 54 | 55 => {
            read_network_nbt(r)?;
        }
        // A single string.
        47 => {
            r.string(MAX_STRING)?;
        }
        // A varint-counted list of anonymous NBT values (the lore lines).
        7 => {
            let count = read_count(r)?;
            for _ in 0..count {
                read_network_nbt(r)?;
            }
        }
        // An enchantment map: `(varint id, varint level)` pairs, then a
        // show-in-tooltip flag.
        9 | 23 => {
            let count = read_count(r)?;
            for _ in 0..count {
                r.var_i32()?;
                r.var_i32()?;
            }
            r.bool()?;
        }
        // A packed colour and a show-in-tooltip flag.
        24 => {
            r.i32()?;
            r.bool()?;
        }
        // Block-state properties: `(name, value)` string pairs.
        52 => {
            let count = read_count(r)?;
            for _ in 0..count {
                r.string(MAX_STRING)?;
                r.string(MAX_STRING)?;
            }
        }
        // A varint-counted list of varints.
        50 => {
            let count = read_count(r)?;
            for _ in 0..count {
                r.var_i32()?;
            }
        }
        // A varint-counted list of nested stacks.
        29 | 30 | 51 => {
            let count = read_count(r)?;
            for _ in 0..count {
                decode_nested(r, ctx, depth + 1)?;
            }
        }
        // Suspicious stew: `(varint effect, varint duration)` pairs.
        32 => {
            let count = read_count(r)?;
            for _ in 0..count {
                r.var_i32()?;
                r.var_i32()?;
            }
        }
        // Everything else — see the module docs. Each is a nested structure
        // reaching a registry holder, a block predicate or an attribute
        // record this crate has no model for, and a payload whose length
        // cannot be derived must not be guessed at.
        other => {
            return Err(Error::InvalidEnumVariant {
                name: "1.20.6 item component payload",
                value: other,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod nesting_budget {
    //! Three component payloads at this protocol hold whole nested stacks, and
    //! the wire says nothing about how deeply they nest — that is the sender's
    //! choice. These gates pin both halves of [`MAX_ITEM_NESTING`]: that a
    //! stack nested to the cap still decodes, so the cap is reachable rather
    //! than a stack overflow behind an accepted input, and that one level past
    //! it is refused by the bound rather than by a short read.

    use super::{MAX_ITEM_NESTING, Slot};
    use lodestone_core::{Ctx, Decode, Reader};

    /// `minecraft:bundle_contents` — one of the three payloads whose value is a
    /// list of nested stacks, per this module's own component table.
    const BUNDLE_CONTENTS: u8 = 51;

    /// A `Slot` nested `levels` deep through `bundle_contents`, the innermost
    /// stack carrying no components.
    fn nested(levels: u32) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..levels.saturating_sub(1) {
            out.push(1); // count, non-zero so a stack follows
            out.push(0); // item id
            out.push(1); // one added component
            out.push(0); // no removed components
            out.push(BUNDLE_CONTENTS);
            out.push(1); // one nested stack in the list
        }
        out.extend_from_slice(&[1, 0, 0, 0]); // innermost: no components
        out
    }

    fn decode(bytes: &[u8]) -> lodestone_core::Result<Slot> {
        Slot::decode(&mut Reader::new(bytes), Ctx { version: 766 })
    }

    #[test]
    fn a_stack_nested_to_the_cap_still_decodes() {
        let bytes = nested(MAX_ITEM_NESTING);
        assert!(
            decode(&bytes).is_ok(),
            "a stack nested to the cap of {MAX_ITEM_NESTING} was refused: {:?} — a cap the \
             decoder cannot itself reach is a stack overflow behind an accepted input, not a \
             bound",
            decode(&bytes)
        );
    }

    #[test]
    fn a_stack_nested_past_the_cap_is_refused_by_the_bound() {
        let error = decode(&nested(MAX_ITEM_NESTING + 1))
            .expect_err("a stack nested past the cap was accepted");
        assert!(
            error.to_string().contains("nests deeper"),
            "past the cap this failed for some other reason than the nesting bound, so the \
             input proves nothing about the bound: {error}"
        );
    }

    /// The control for the two gates above: a generator that did not actually
    /// nest would satisfy both of them vacuously.
    #[test]
    fn the_generator_actually_nests() {
        let one = nested(1);
        let two = nested(2);
        assert_eq!(one, vec![1, 0, 0, 0], "one level is a stack with no components");
        assert!(two.len() > one.len() && decode(&two).is_ok(), "got {two:?}");
    }
}
