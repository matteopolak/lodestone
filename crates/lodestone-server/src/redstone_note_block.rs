//! Note blocks (`minecraft:note_block`) combine instrument selection, redstone
//! edge detection, and pitch cycling.
//!
//! # What it is
//!
//! Three independent pieces define note-block behavior:
//!
//! 1. **Instrument selection** ([`instrument_for_note_block`]) — one of the 27
//!    [`Instrument`] values, selected from the blocks directly above and below.
//! 2. **The redstone pulse** ([`on_neighbor_changed`]) — `POWERED` follows the
//!    neighbour signal, and its rising edge conditionally plays a note.
//! 3. **Note cycling** ([`cycle_note`]) — a right-click without a
//!    top-instrument item advances the pitch by one semitone, wrapping.
//!
//! # What this needs of the execution model
//!
//! * **Trigger**: a neighbour notification; [`on_neighbor_changed`] makes the
//!   decision in the same tick through `react_to_notification`.
//! * **Propagation**: none beyond the block's own `POWERED` write reaching the
//!   client. A note block is not a signal source, so no further notification is
//!   required.
//! * **Scheduled tick**: none.
//! * **Client effects**: the boolean returned by [`on_neighbor_changed`]
//!   identifies a pulse, while the state-diff event carries the transition
//!   inputs needed by [`played_pulse_on_transition`]. The redstone propagation
//!   path posts `VibrationEvent::NoteBlockPlay` for listeners such as allays.
//!   Audible sound and particles require a client-visible block-action message
//!   because they do not change block state.
//! * **Right-click cycling** ([`cycle_note`]) is dispatched by
//!   `hand_use::hand_use`. That path updates the pitch but does not post a
//!   vibration; only redstone-triggered pulses reach vibration listeners.

use crate::redstone::{base_name, get_bool_property, get_u32_property, with_property};

pub const NOTE_BLOCK: &str = "minecraft:note_block";

/// The 27 note-block instruments in declaration order. The discriminant is
/// not used numerically; [`Self::works_above_note_block`] is the behavioral
/// distinction needed by this module.
///
/// The four `Trumpet*` variants have no entry in [`block_instrument`]'s table
/// (`#[allow(dead_code)]` on them). They remain available so a caller reading
/// [`instrument_property`] from a state string can resolve every registered
/// value, even though the table does not emit those variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instrument {
    Harp,
    Basedrum,
    Snare,
    Hat,
    Bass,
    Flute,
    Bell,
    Guitar,
    Chime,
    Xylophone,
    IronXylophone,
    CowBell,
    Didgeridoo,
    Bit,
    Banjo,
    Pling,
    #[allow(dead_code)]
    Trumpet,
    #[allow(dead_code)]
    TrumpetExposed,
    #[allow(dead_code)]
    TrumpetOxidized,
    #[allow(dead_code)]
    TrumpetWeathered,
    Zombie,
    Skeleton,
    Creeper,
    Dragon,
    WitherSkeleton,
    Piglin,
    CustomHead,
}

impl Instrument {
    /// `NoteBlockInstrument.worksAboveNoteBlock` (`:64-66`) — `true` for
    /// every `MOB_HEAD`/`CUSTOM` type, `false` for every `BASE_BLOCK` type.
    /// The distinction is what `setInstrument` uses to prefer a mob head
    /// placed above a note block over the plain block underneath it.
    #[must_use]
    pub fn works_above_note_block(self) -> bool {
        matches!(
            self,
            Instrument::Zombie
                | Instrument::Skeleton
                | Instrument::Creeper
                | Instrument::Dragon
                | Instrument::WitherSkeleton
                | Instrument::Piglin
                | Instrument::CustomHead
        )
    }

    /// The `INSTRUMENT` block-state property's wire value — the exact reverse
    /// of [`instrument_property`], so a value this crate writes always reads
    /// back to the same variant. `crate::block_placement::placement`'s
    /// `minecraft:note_block` arm is the writer.
    #[must_use]
    pub fn state_name(self) -> &'static str {
        match self {
            Instrument::Harp => "harp",
            Instrument::Basedrum => "basedrum",
            Instrument::Snare => "snare",
            Instrument::Hat => "hat",
            Instrument::Bass => "bass",
            Instrument::Flute => "flute",
            Instrument::Bell => "bell",
            Instrument::Guitar => "guitar",
            Instrument::Chime => "chime",
            Instrument::Xylophone => "xylophone",
            Instrument::IronXylophone => "iron_xylophone",
            Instrument::CowBell => "cow_bell",
            Instrument::Didgeridoo => "didgeridoo",
            Instrument::Bit => "bit",
            Instrument::Banjo => "banjo",
            Instrument::Pling => "pling",
            Instrument::Trumpet => "trumpet",
            Instrument::TrumpetExposed => "trumpet_exposed",
            Instrument::TrumpetOxidized => "trumpet_oxidized",
            Instrument::TrumpetWeathered => "trumpet_weathered",
            Instrument::Zombie => "zombie",
            Instrument::Skeleton => "skeleton",
            Instrument::Creeper => "creeper",
            Instrument::Dragon => "dragon",
            Instrument::WitherSkeleton => "wither_skeleton",
            Instrument::Piglin => "piglin",
            Instrument::CustomHead => "custom_head",
        }
    }
}

/// `BlockState.instrument()`'s per-block table, for exactly the blocks this
/// module has verified against vanilla's own block registration table's `.instrument(...)`
/// registrations: the 9 single-block overrides plus the 7 head blocks (heads
/// only, since a wall-mounted skull cannot be placed *on top of* a note block
/// in the first place) plus the small `SNARE` family (7 blocks — sand/gravel
/// and their two suspicious variants, a shulker box, and the heavy core).
///
/// **Not exhaustive.** The two large families — `BASS` (~190 registrations:
/// every wood-family block) and `BASEDRUM` (~140: every stone-family block) —
/// are not enumerated here; a note block sitting on oak planks reads `Harp`
/// (this function's fallback) rather than vanilla's `Bass` until that table is
/// built, which is a `lodestone-data` generated-census task (the same shape as
/// `block_items`/`entity_types`) rather than something to hand-roll in a
/// redstone module. Every entry that *is* present here was extracted from a
/// literal `Blocks.<NAME> = register(..., instrument(NoteBlockInstrument.X)
/// ...)` call, not guessed.
///
/// Wired in from `crate::block_placement::placement`'s `minecraft:note_block`
/// arm via [`instrument_for_note_block`].
#[must_use]
pub fn block_instrument(block: &str) -> Instrument {
    match base_name(block) {
        "minecraft:gold_block" => Instrument::Bell,
        "minecraft:iron_block" => Instrument::IronXylophone,
        "minecraft:clay" => Instrument::Flute,
        "minecraft:soul_sand" => Instrument::CowBell,
        "minecraft:glowstone" => Instrument::Pling,
        "minecraft:pumpkin" => Instrument::Didgeridoo,
        "minecraft:emerald_block" => Instrument::Bit,
        "minecraft:hay_block" => Instrument::Banjo,
        "minecraft:packed_ice" => Instrument::Chime,
        "minecraft:bone_block" => Instrument::Xylophone,
        "minecraft:skeleton_skull" => Instrument::Skeleton,
        "minecraft:wither_skeleton_skull" => Instrument::WitherSkeleton,
        "minecraft:zombie_head" => Instrument::Zombie,
        "minecraft:player_head" => Instrument::CustomHead,
        "minecraft:creeper_head" => Instrument::Creeper,
        "minecraft:dragon_head" => Instrument::Dragon,
        "minecraft:piglin_head" => Instrument::Piglin,
        "minecraft:sand"
        | "minecraft:suspicious_sand"
        | "minecraft:red_sand"
        | "minecraft:gravel"
        | "minecraft:suspicious_gravel"
        | "minecraft:shulker_box"
        | "minecraft:heavy_core" => Instrument::Snare,
        // Vanilla's own default for a block with no `.instrument(...)` call is
        // `NoteBlockInstrument.HARP` — see `BlockBehaviour.Properties`'s field
        // default. So the fallback here is correct for "genuinely unmodelled",
        // and wrong only for the BASS/BASEDRUM families named above.
        _ => Instrument::Harp,
    }
}

/// Vanilla's own `NoteBlock.setInstrument` — the block directly
/// above wins if its instrument `worksAboveNoteBlock` (a mob head sitting on
/// top), otherwise the block below is read, with its own
/// `worksAboveNoteBlock` guarded back to [`Instrument::Harp`] (vanilla's
/// defensive case for a head somehow ending up *below* a note block).
#[must_use]
pub fn instrument_for_note_block(above: &str, below: &str) -> Instrument {
    let above_instrument = block_instrument(above);
    if above_instrument.works_above_note_block() {
        return above_instrument;
    }
    let below_instrument = block_instrument(below);
    if below_instrument.works_above_note_block() {
        Instrument::Harp
    } else {
        below_instrument
    }
}

/// The result of a neighbour notification reaching a note block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeighborReaction {
    /// The state to write — `POWERED` flipped to match `has_signal`.
    pub new_state: String,
    /// Vanilla's own `playNote` gate: fire only on the
    /// *rising* edge, and only when the instrument works standing on its own
    /// (a mob head) or the cell directly above is air. A note block buried
    /// under a solid block stays silent even while it dutifully tracks
    /// `POWERED`.
    pub play_pulse: bool,
}

/// Vanilla's own `NoteBlock.neighborChanged`. `has_signal` is
/// vanilla's `level.hasNeighborSignal(pos)` — the caller supplies
/// `crate::redstone::best_neighbor_signal(lookup, pos, false) > 0`, exactly
/// the expression `crate::random_tick`'s hopper `ENABLED` arm already
/// computes for the identical vanilla method. `None` when `state` is not a
/// note block, or when `has_signal` already matches the current `POWERED`
/// (vanilla's own `if (signal != state.getValue(POWERED))` guard — nothing to
/// write, nothing to fan out).
#[must_use]
pub fn on_neighbor_changed(state: &str, has_signal: bool, above_is_air: bool) -> Option<NeighborReaction> {
    if base_name(state) != NOTE_BLOCK {
        return None;
    }
    let was_powered = get_bool_property(state, "powered").unwrap_or(false);
    if has_signal == was_powered {
        return None;
    }
    let instrument = instrument_property(state);
    let play_pulse = has_signal && (instrument.works_above_note_block() || above_is_air);
    Some(NeighborReaction {
        new_state: with_property(state, "powered", if has_signal { "true" } else { "false" }),
        play_pulse,
    })
}

/// Re-derives [`NeighborReaction::play_pulse`] from the **already-written**
/// before/after states of a note block, for a caller that only has a
/// [`crate::random_tick::RandomTickEvent`]'s `(from, to)` pair rather than
/// the [`NeighborReaction`] this module built it from — every one of
/// `crate::tick`'s four `propagate_and_react_with_entities` consumers is
/// exactly that caller, and this is what closes the gap this module's own
/// doc used to call unclosable ("nowhere in this event type to put it"):
/// **re-verified false** — a `RandomTickEvent`'s two state strings already
/// carry every input `playNote`'s own gate needs (`POWERED`'s rising edge,
/// off `from`/`to`; `INSTRUMENT`, off `to`, unchanged by a `POWERED` flip),
/// so nothing needed to be added to that struct at all. `above_is_air` is
/// still an environmental read the caller must supply — a block above the
/// note block is not part of its own state string.
///
/// Reproduces exactly the same boolean [`on_neighbor_changed`] computes so
/// the two cannot silently drift — see this crate's own evidence standard on
/// two derivations of one rule needing to agree; `tests::transition_agrees_with_on_neighbor_changed`
/// is the check.
#[must_use]
pub fn played_pulse_on_transition(from: &str, to: &str, above_is_air: bool) -> bool {
    if base_name(to) != NOTE_BLOCK {
        return false;
    }
    let was_powered = get_bool_property(from, "powered").unwrap_or(false);
    let is_powered = get_bool_property(to, "powered").unwrap_or(false);
    if !is_powered || is_powered == was_powered {
        return false;
    }
    let instrument = instrument_property(to);
    instrument.works_above_note_block() || above_is_air
}

/// The `NOTE` property's 25 values (`0..=24`, `BlockStateProperties.NOTE`).
const NOTE_COUNT: u32 = 25;

/// `BlockState.cycle(NOTE)` as vanilla's own empty-hand-use handler calls it
/// — advance the pitch by one semitone, wrapping `24`
/// back to `0`. `None` when `state` is not a note block.
///
/// Called from `hand_use::hand_use`'s note-block arm, the right-click
/// dispatcher this module does not own — see this module's own doc comment.
#[must_use]
pub fn cycle_note(state: &str) -> Option<String> {
    if base_name(state) != NOTE_BLOCK {
        return None;
    }
    let current = get_u32_property(state, "note").unwrap_or(0).min(NOTE_COUNT - 1);
    let next = (current + 1) % NOTE_COUNT;
    Some(with_property(state, "note", &next.to_string()))
}

/// The `INSTRUMENT` property already written onto a note block's own state
/// string — read back with the same `Instrument::Harp` default
/// [`instrument_for_note_block`] would have written for an unresolved block.
fn instrument_property(state: &str) -> Instrument {
    match crate::redstone::get_str_property(state, "instrument") {
        Some("basedrum") => Instrument::Basedrum,
        Some("snare") => Instrument::Snare,
        Some("hat") => Instrument::Hat,
        Some("bass") => Instrument::Bass,
        Some("flute") => Instrument::Flute,
        Some("bell") => Instrument::Bell,
        Some("guitar") => Instrument::Guitar,
        Some("chime") => Instrument::Chime,
        Some("xylophone") => Instrument::Xylophone,
        Some("iron_xylophone") => Instrument::IronXylophone,
        Some("cow_bell") => Instrument::CowBell,
        Some("didgeridoo") => Instrument::Didgeridoo,
        Some("bit") => Instrument::Bit,
        Some("banjo") => Instrument::Banjo,
        Some("pling") => Instrument::Pling,
        Some("zombie") => Instrument::Zombie,
        Some("skeleton") => Instrument::Skeleton,
        Some("creeper") => Instrument::Creeper,
        Some("dragon") => Instrument::Dragon,
        Some("wither_skeleton") => Instrument::WitherSkeleton,
        Some("piglin") => Instrument::Piglin,
        Some("custom_head") => Instrument::CustomHead,
        _ => Instrument::Harp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The nine single-block overrides, each pinned against the exact
    /// vanilla registration line in its own block registration table — a magnitude check, not merely
    /// "changed", per this crate's own evidence standard.
    #[test]
    fn the_nine_single_block_overrides_match_their_jar_registrations() {
        assert_eq!(block_instrument("minecraft:gold_block"), Instrument::Bell);
        assert_eq!(block_instrument("minecraft:iron_block"), Instrument::IronXylophone);
        assert_eq!(block_instrument("minecraft:clay"), Instrument::Flute);
        assert_eq!(block_instrument("minecraft:soul_sand"), Instrument::CowBell);
        assert_eq!(block_instrument("minecraft:glowstone"), Instrument::Pling);
        assert_eq!(block_instrument("minecraft:pumpkin"), Instrument::Didgeridoo);
        assert_eq!(block_instrument("minecraft:emerald_block"), Instrument::Bit);
        assert_eq!(block_instrument("minecraft:hay_block"), Instrument::Banjo);
        assert_eq!(block_instrument("minecraft:packed_ice"), Instrument::Chime);
        assert_eq!(block_instrument("minecraft:bone_block"), Instrument::Xylophone);
    }

    #[test]
    fn heads_read_their_own_mob_instrument_and_a_plain_block_falls_back_to_harp() {
        assert_eq!(block_instrument("minecraft:creeper_head"), Instrument::Creeper);
        assert_eq!(block_instrument("minecraft:dragon_head"), Instrument::Dragon);
        assert_eq!(block_instrument("minecraft:player_head"), Instrument::CustomHead);
        assert_eq!(block_instrument("minecraft:stone"), Instrument::Harp, "unmodelled families fall back to vanilla's own default");
    }

    /// **The conjunction this module's own doc comment warns about**: a head
    /// above wins even when the block below is also head-shaped, and a head
    /// below (the defensive branch) is overridden back to `Harp` rather than
    /// leaking its `worksAboveNoteBlock` instrument upward.
    #[test]
    fn a_head_above_wins_and_a_head_below_is_guarded_back_to_harp() {
        assert_eq!(
            instrument_for_note_block("minecraft:creeper_head", "minecraft:gold_block"),
            Instrument::Creeper,
            "the block above wins whenever it works standing alone"
        );
        assert_eq!(
            instrument_for_note_block("minecraft:air", "minecraft:creeper_head"),
            Instrument::Harp,
            "a head below is the defensive branch: guarded back to Harp, not Creeper"
        );
        assert_eq!(
            instrument_for_note_block("minecraft:air", "minecraft:gold_block"),
            Instrument::Bell,
            "an ordinary base-block below is read directly when nothing sits above"
        );
    }

    #[test]
    fn note_cycles_through_all_twenty_five_values_and_wraps() {
        let mut state = "minecraft:note_block[instrument=harp,note=0,powered=false]".to_string();
        for expected in 1..25 {
            state = cycle_note(&state).expect("a note block");
            assert_eq!(get_u32_property(&state, "note"), Some(expected));
        }
        // One more cycle from 24 wraps back to 0 — the discriminating step, since
        // a naive `+1` with no modulus would instead read 25.
        state = cycle_note(&state).expect("a note block");
        assert_eq!(get_u32_property(&state, "note"), Some(0));
    }

    /// The rising edge plays a pulse (buried or not decides *whether*), the
    /// falling edge never does, and no-change is a no-op — three arms that a
    /// single boolean-toggle implementation could not distinguish.
    #[test]
    fn only_the_rising_edge_can_pulse_and_only_when_audible() {
        let unpowered = "minecraft:note_block[instrument=harp,note=0,powered=false]";
        let powered = "minecraft:note_block[instrument=harp,note=0,powered=true]";

        let exposed = on_neighbor_changed(unpowered, true, true).expect("signal arrived");
        assert_eq!(exposed.new_state, "minecraft:note_block[instrument=harp,note=0,powered=true]");
        assert!(exposed.play_pulse, "air above: the pulse is audible, so it must fire");

        let buried = on_neighbor_changed(unpowered, true, false).expect("signal arrived");
        assert!(!buried.play_pulse, "a Harp note block buried under a solid block must stay silent");

        let falling = on_neighbor_changed(powered, false, true).expect("signal left");
        assert!(!falling.play_pulse, "the falling edge never plays a note, audible or not");

        assert_eq!(on_neighbor_changed(powered, true, true), None, "no change means no reaction at all");
    }

    /// A mob-head instrument pulses even buried, because `worksAboveNoteBlock`
    /// bypasses the "air above" requirement entirely — the other half of the
    /// `play_pulse` disjunction, which the buried-Harp case above cannot
    /// exercise on its own.
    #[test]
    fn a_mob_head_instrument_pulses_even_when_buried() {
        let unpowered = "minecraft:note_block[instrument=creeper,note=0,powered=false]";
        let reaction = on_neighbor_changed(unpowered, true, false).expect("signal arrived");
        assert!(reaction.play_pulse, "a head instrument works standing alone, air above or not");
    }

    /// [`played_pulse_on_transition`] must agree with [`on_neighbor_changed`]'s
    /// own `play_pulse` on every arm the latter test already exercises — the
    /// two-derivations-of-one-rule check this crate's own evidence standard
    /// asks for, so the two functions cannot silently drift apart.
    #[test]
    fn transition_agrees_with_on_neighbor_changed() {
        let cases: &[(&str, bool, bool)] = &[
            ("minecraft:note_block[instrument=harp,note=0,powered=false]", true, true),
            ("minecraft:note_block[instrument=harp,note=0,powered=false]", true, false),
            ("minecraft:note_block[instrument=harp,note=0,powered=true]", false, true),
            ("minecraft:note_block[instrument=creeper,note=0,powered=false]", true, false),
        ];
        let mut mismatches = Vec::new();
        for &(state, has_signal, above_is_air) in cases {
            let Some(reaction) = on_neighbor_changed(state, has_signal, above_is_air) else {
                continue;
            };
            let via_transition = played_pulse_on_transition(state, &reaction.new_state, above_is_air);
            if via_transition != reaction.play_pulse {
                mismatches.push((state, has_signal, above_is_air, reaction.play_pulse, via_transition));
            }
        }
        assert!(
            mismatches.is_empty(),
            "played_pulse_on_transition disagreed with on_neighbor_changed: {mismatches:?}"
        );
    }
}
