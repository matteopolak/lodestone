//! Resolving a command block the player is looking at into the data the edit
//! screen opens with (a missing trigger, tracked as a follow-up).
//!
//! # What this closes
//!
//! `Screen::CommandBlockEdit`, `command_block::CommandBlockState` and
//! `render::command_block_frame` landed in `c76510b` real and unit-tested, and
//! **nothing opened them**: `UiState::open_command_block` and
//! `MenuNav::open_command_block` had no production caller. That fix's ledger
//! entry says the lift is "substantially bigger than the one-line hookup the
//! issue brief anticipated, since the data to open the screen *with* does not
//! exist yet either", citing `grep -rn "CommandBlock" crates/lodestone-model
//! crates/protocol` finding nothing.
//!
//! **That grep is answering a neighbouring question.** There is indeed no
//! *typed* command-block decode in `lodestone-model` or `crates/protocol` — and
//! there does not need to be. `lodestone_world::BlockEntity` already carries the
//! server's raw NBT payload for every block entity in a loaded chunk, fed by
//! all four of the creation routes `crate::block_entities`' module doc
//! enumerates, and `lodestone_data::block_states` already answers what block
//! sits at a position. Between them the screen's whole input set is reachable
//! today. That is the same shape as `SignText`, which reads its lines straight
//! out of `BlockEntity::nbt` with no typed protocol decode anywhere.
//!
//! So this module is a **reader**, not a decoder, and it deliberately lives in
//! the shell rather than in `lodestone-model`: nothing outside the edit screen
//! wants this data, and a wire-shaped type for it would be a type with one
//! consumer.
//!
//! # The block state is the truth, the NBT is the payload
//!
//! Exactly the split `crate::block_entities` documents for chests. Vanilla
//! itself reads the two separately:
//!
//! * **Mode** comes from the *block*, not the NBT —
//!   vanilla's own command-block-entity mode lookup matches
//!   on `minecraft:command_block` / `minecraft:repeating_command_block` /
//!   `minecraft:chain_command_block`. There is no mode field on the wire at all, which is
//!   why a reader that only looked at NBT would show every chain block as
//!   Redstone.
//! * **Conditional** likewise — vanilla's own command-block-entity
//!   is-conditional check reads the block state's own conditional property.
//! * **Command / TrackOutput / LastOutput / auto** come from the NBT
//!   (vanilla's own base-command-block save routine, and
//!   its own command-block-entity save-additional routine).
//!
//! # Fail open, never fail blank
//!
//! Every read here degrades to vanilla's own field initialiser rather than
//! refusing to open. A server that has not sent the block entity's data yet
//! (`Nbt::End`, the common case for a freshly placed block) opens the screen
//! with an empty command, which is exactly what vanilla shows — the screen is
//! an editor, not a viewer, and refusing to open would be a dead control.

use lodestone_core::Nbt;
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, CommandBlockMode};

use crate::menu::command_block::CommandBlockOpen;

/// The three command-block blocks, and the mode each one *is*.
///
/// Ordered as vanilla's own command-block-entity mode lookup tests them. Note the mapping is not
/// alphabetical or intuitive: the plain `command_block` is **Redstone**, the
/// *repeating* one is **Auto**, and the *chain* one is **Sequence**.
const COMMAND_BLOCKS: [(&str, CommandBlockMode); 3] = [
    ("minecraft:command_block", CommandBlockMode::Redstone),
    ("minecraft:repeating_command_block", CommandBlockMode::Auto),
    ("minecraft:chain_command_block", CommandBlockMode::Sequence),
];

/// The mode for `state`, or `None` if it is not a command block at all.
///
/// # `block_name`, and why a block-type lookup here was a live bug
///
/// [`block_name`](lodestone_data::block_states::block_name) is the accessor
/// keyed by **block-state** id — the id space a chunk-section palette and
/// `World::block_at` deal in, and the one `state` is. It already returns the
/// block's own identifier rather than the state's, so it is stable across
/// `facing`/`conditional`, which is the property the three-way match needs.
///
/// A block-type lookup's parameter is a
/// **`minecraft:block` registry** id (1,196 entries, registration order) and
/// *not* a state id — its own doc says so. The two spaces are unrelated orders,
/// so passing a state id in resolves an arbitrary block. The discriminating
/// values show both directions: real command-block states 9968, 14817 and
/// 14829 fall past `BLOCK_COUNT` and answer `None`, while the three registry ids
/// reused as state ids answer `Some` — state 407 is `minecraft:cherry_leaves`
/// (→ Redstone) and 668/669 are `minecraft:note_block` (→ Auto/Sequence).
///
/// The test selects its subject in the state-id space and checks the real
/// command block (`Some(Redstone)`), while its second half additionally pins
/// the false-positive direction, which no positive assertion can see.
#[must_use]
pub fn mode_for_state(state: StateId) -> Option<CommandBlockMode> {
    let name = state.name();
    COMMAND_BLOCKS
        .iter()
        .find(|(id, _)| *id == name)
        .map(|(_, mode)| *mode)
}

/// Whether `state`'s `conditional` property is set —
/// vanilla's own conditional block-state property.
///
/// Absent property reads `false`, matching
/// vanilla's own command-block-entity is-conditional check's own fallback for a block that is not
/// a command block.
#[must_use]
fn conditional_for_state(state: StateId) -> bool {
    state
        .properties()
        .iter()
        .any(|(key, value)| *key == "conditional" && *value == "true")
}

/// Look up `key` in an NBT compound's field list.
///
/// A linear scan, like `lodestone-world`'s own sign parser: a command block's
/// compound has single-digit field counts, and building a map to read four
/// fields once per screen-open would cost more than it saves.
fn find<'a>(fields: &'a [(String, Nbt)], key: &str) -> Option<&'a Nbt> {
    fields
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

/// NBT has no boolean type — vanilla writes `putBoolean` as a `Byte`, so a
/// non-zero byte is `true`. Reading only `Nbt::Byte` and not, say, `Int` is
/// deliberate: a differently-typed field means the payload is not the shape
/// this expects, and silently coercing it would hide that.
fn as_bool(nbt: &Nbt) -> Option<bool> {
    match nbt {
        Nbt::Byte(b) => Some(*b != 0),
        _ => None,
    }
}

fn as_string(nbt: &Nbt) -> Option<&str> {
    match nbt {
        Nbt::String(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Build the screen's opening state from a command block's position, block
/// state and raw block-entity NBT. `state` is validated at the chunk-snapshot
/// boundary, before this source module receives it.
///
/// Returns `None` only when `state_id` is not a command block — the one case
/// where opening the screen would be wrong. Every *data* problem below that
/// (no NBT at all, a malformed compound, a missing field, a wrongly-typed
/// field) degrades to vanilla's own default for that field.
#[must_use]
pub fn command_block_open(pos: BlockPos, state: StateId, nbt: &Nbt) -> Option<CommandBlockOpen> {
    let mode = mode_for_state(state)?;
    let conditional = conditional_for_state(state);

    // `Nbt::End` is what `BlockEntity::nbt` holds for a block the server sent
    // no data for, which is the common case for one the player just placed.
    // That is not an error: vanilla opens an empty editor.
    let fields: &[(String, Nbt)] = match nbt {
        Nbt::Compound(fields) => fields,
        _ => &[],
    };

    // Vanilla's own base-command-block load routine's defaults: `Command` to
    // `""`, `TrackOutput` to
    // **`true`** — note the asymmetry, it is
    // the one field here whose default is not `false`/empty.
    let command = find(fields, "Command")
        .and_then(as_string)
        .unwrap_or_default()
        .to_owned();
    let track_output = find(fields, "TrackOutput").and_then(as_bool).unwrap_or(true);
    let automatic = find(fields, "auto").and_then(as_bool).unwrap_or(false);

    // `LastOutput` is a chat `Component`, and vanilla only shows it when
    // tracking is on (vanilla's own abstract command-block-edit screen sets the
    // previous-output line to `"-"` otherwise). Drawn as `"-"` by the screen
    // when `None`, so an absent or untracked output needs no special case here.
    let previous_output = track_output
        .then(|| find(fields, "LastOutput"))
        .flatten()
        .map(lodestone_core::plain_text_from_nbt_component)
        .filter(|text| !text.is_empty());

    Some(CommandBlockOpen {
        pos,
        command,
        track_output,
        previous_output,
        mode,
        conditional,
        automatic,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compound(fields: Vec<(&str, Nbt)>) -> Nbt {
        Nbt::Compound(
            fields
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
        )
    }

    /// The first **block-state** id belonging to `name`, panicking rather than
    /// skipping if the table has no such block.
    ///
    /// `block_name`, not a block-type lookup: see [`mode_for_state`]'s doc. The
    /// Selecting the subject through a block-type id would make the helper use
    /// the wrong id space as the code under test.
    ///
    /// A `find` that returns `None` used to `continue`/`return` — the
    /// *precondition* species of vacuous test, green when it measured nothing.
    /// These are all vanilla blocks in a generated 26.2 table; absence is a
    /// broken table, not a case to skip.
    fn state_of(name: &str) -> StateId {
        (0u32..lodestone_data::block_states::STATE_COUNT as u32)
            .find(|id| lodestone_data::block_states::block_name(*id).is_some_and(|n| n == name))
            .and_then(StateId::new)
            .unwrap_or_else(|| panic!("the block-state table must contain {name}"))
    }

    /// A state id that is definitely not a command block, resolved from the
    /// real table rather than guessed — `0` happens to be air, but asserting
    /// that would be asserting a table detail this test does not care about.
    fn non_command_block_state() -> StateId {
        state_of("minecraft:stone")
    }

    fn command_block_state() -> StateId {
        state_of("minecraft:command_block")
    }

    /// **The mode comes from the block, not the NBT** — the trap this module's
    /// doc names. A reader that only looked at the payload would report every
    /// chain block as Redstone, which is a plausible-looking wrong answer.
    ///
    /// The second half is the control for the id-space bug [`mode_for_state`]'s
    /// doc describes, and it is deliberately **not** built from anything this
    /// module chooses: it takes each command block's `minecraft:block`
    /// **registry** id — the number the broken call was really indexing — and
    /// requires that same number, read as a *state* id, to answer `None`. Under
    /// the bug those three ids are exactly the ones that answered `Some`
    /// (cherry leaves and note blocks), so this fails there and passes here.
    /// Asserting only the positive half cannot see it: the positive half was
    /// *also* green under the bug, because it picked its subject the same wrong
    /// way.
    #[test]
    fn each_command_block_reports_its_own_mode_and_a_normal_block_reports_none() {
        for (name, expected) in COMMAND_BLOCKS {
            assert_eq!(
                mode_for_state(state_of(name)),
                Some(expected),
                "{name} must report its own mode"
            );
        }
        assert_eq!(
            mode_for_state(non_command_block_state()),
            None,
            "stone is not a command block, and must not open the screen"
        );

        for (name, _) in COMMAND_BLOCKS {
            let registry_id = lodestone_data::block::Block::from_name(name)
                .unwrap_or_else(|| panic!("the block registry must contain {name}"))
                .registry_id() as u32;
            let as_a_state = StateId::new(registry_id).map(StateId::name);
            assert_ne!(
                as_a_state,
                Some(name),
                "premise: {name}'s registry id {registry_id} must not also be one of \
                 its state ids, or this control cannot distinguish the two spaces"
            );
            assert_eq!(
                StateId::new(registry_id).and_then(mode_for_state),
                None,
                "control: {name}'s *registry* id {registry_id} is block-state id \
                 {as_a_state:?}, which is not a command block — reading a state id \
                 through the registry table is the bug that made the edit screen \
                 unopenable on real command blocks and openable on these"
            );
        }
    }

    /// The public helpers accept an already-validated state and remain total
    /// with respect to its numeric range. A real but unrelated state still
    /// answers `None`: that is the command-block classification result, not a
    /// failed numeric lookup.
    #[test]
    fn command_block_helpers_require_validated_state_ids() {
        let _: fn(StateId) -> Option<CommandBlockMode> = mode_for_state;
        let _: fn(BlockPos, StateId, &Nbt) -> Option<CommandBlockOpen> = command_block_open;

        let first = StateId::new(0).expect("the first generated state is valid");
        let last = StateId::new(lodestone_data::block_states::STATE_COUNT - 1)
            .expect("the final generated state is valid");
        assert!(StateId::new(lodestone_data::block_states::STATE_COUNT).is_none());
        assert!(StateId::new(u32::MAX).is_none());
        assert!(mode_for_state(first).is_none(), "air is not a command block");
        let expected_last = COMMAND_BLOCKS
            .iter()
            .find(|(name, _)| last.name() == *name)
            .map(|(_, mode)| *mode);
        assert_eq!(mode_for_state(last), expected_last);
    }

    /// **`TrackOutput` defaults to `true`, unlike every other flag here.**
    ///
    /// Predicts the exact value rather than asserting "some default": a reader
    /// that defaulted the whole struct to `false` would be wrong in one field
    /// only, and that is precisely the field an unaware port gets wrong.
    #[test]
    fn an_empty_payload_opens_with_vanillas_own_field_initialisers() {
        let id = command_block_state();
        let pos = BlockPos::new(3, 64, -7);
        let open = command_block_open(pos, id, &Nbt::End)
            .expect("a command block with no payload must still open");

        assert_eq!(open.pos, pos);
        assert_eq!(open.command, "", "no Command field => empty");
        assert!(
            open.track_output,
            "BaseCommandBlock.load defaults TrackOutput to TRUE — the one \
             field here whose default is not false"
        );
        assert!(!open.automatic, "no `auto` field => false");
        assert_eq!(open.previous_output, None);
        assert_eq!(open.mode, CommandBlockMode::Redstone);
    }

    /// The payload's fields must actually be read, not merely defaulted — the
    /// control for the test above.
    #[test]
    fn a_real_payload_overrides_every_default_it_carries() {
        let id = command_block_state();
        let nbt = compound(vec![
            ("Command", Nbt::String("say hello".into())),
            ("TrackOutput", Nbt::Byte(0)),
            ("auto", Nbt::Byte(1)),
        ]);
        let open = command_block_open(BlockPos::new(0, 0, 0), id, &nbt).expect("command block");

        assert_eq!(open.command, "say hello");
        assert!(
            !open.track_output,
            "an explicit TrackOutput=0 must beat the `true` default — if this \
             reads true, the field is not being read at all"
        );
        assert!(open.automatic);
    }

    /// `LastOutput` is suppressed while tracking is off, matching vanilla's own
    /// `"-"` placeholder, and a malformed payload never refuses to open.
    #[test]
    fn last_output_is_hidden_while_untracked_and_junk_still_opens() {
        let id = command_block_state();
        let with_output = |track: i8| {
            compound(vec![
                ("TrackOutput", Nbt::Byte(track)),
                (
                    "LastOutput",
                    compound(vec![("text", Nbt::String("done".into()))]),
                ),
            ])
        };

        let tracked = command_block_open(BlockPos::new(0, 0, 0), id, &with_output(1))
            .expect("command block");
        assert_eq!(
            tracked.previous_output.as_deref(),
            Some("done"),
            "a tracked output must reach the screen"
        );

        let untracked = command_block_open(BlockPos::new(0, 0, 0), id, &with_output(0))
            .expect("command block");
        assert_eq!(
            untracked.previous_output, None,
            "with tracking off the screen draws vanilla's \"-\", so the value \
             must not be carried"
        );

        // Fail open: a compound of the wrong shape entirely.
        let junk = compound(vec![("Command", Nbt::Int(7))]);
        let opened = command_block_open(BlockPos::new(0, 0, 0), id, &junk)
            .expect("a malformed payload must still open the editor, not refuse");
        assert_eq!(
            opened.command, "",
            "a wrongly-typed Command degrades to empty rather than being coerced"
        );
    }
}
