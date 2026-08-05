//! Resolving a command block the player is looking at into the data the edit
//! screen opens with (issue #47's missing trigger, tracked on #436).
//!
//! # What this closes
//!
//! `Screen::CommandBlockEdit`, `command_block::CommandBlockState` and
//! `render::command_block_frame` landed in `c76510b` real and unit-tested, and
//! **nothing opened them**: `UiState::open_command_block` and
//! `MenuNav::open_command_block` had no production caller. Issue #436's ledger
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
//!   `CommandBlockEntity.getMode()` (`CommandBlockEntity.java:144-153`) matches
//!   on `Blocks.COMMAND_BLOCK` / `REPEATING_COMMAND_BLOCK` /
//!   `CHAIN_COMMAND_BLOCK`. There is no mode field on the wire at all, which is
//!   why a reader that only looked at NBT would show every chain block as
//!   Redstone.
//! * **Conditional** likewise — `CommandBlockEntity.isConditional()`
//!   (`:155-158`) reads `blockState.getValue(CommandBlock.CONDITIONAL)`.
//! * **Command / TrackOutput / LastOutput / auto** come from the NBT
//!   (`BaseCommandBlock.save`, `BaseCommandBlock.java:47-52`, and
//!   `CommandBlockEntity.saveAdditional`, `:69-75`).
//!
//! # Fail open, never fail blank
//!
//! Every read here degrades to vanilla's own field initialiser rather than
//! refusing to open. A server that has not sent the block entity's data yet
//! (`Nbt::End`, the common case for a freshly placed block) opens the screen
//! with an empty command, which is exactly what vanilla shows — the screen is
//! an editor, not a viewer, and refusing to open would be a dead control.

use lodestone_core::Nbt;
use lodestone_model::{BlockPos, CommandBlockMode};

use crate::menu::command_block::CommandBlockOpen;

/// The three command-block blocks, and the mode each one *is*.
///
/// Ordered as `CommandBlockEntity.getMode` tests them. Note the mapping is not
/// alphabetical or intuitive: the plain `command_block` is **Redstone**, the
/// *repeating* one is **Auto**, and the *chain* one is **Sequence**.
const COMMAND_BLOCKS: [(&str, CommandBlockMode); 3] = [
    ("minecraft:command_block", CommandBlockMode::Redstone),
    ("minecraft:repeating_command_block", CommandBlockMode::Auto),
    ("minecraft:chain_command_block", CommandBlockMode::Sequence),
];

/// The mode for `state_id`, or `None` if it is not a command block at all.
///
/// `block_type_name` rather than `block_name`: the former is the block's own
/// id, stable across the state's properties, which is what the three-way match
/// needs.
#[must_use]
pub fn mode_for_state(state_id: u32) -> Option<CommandBlockMode> {
    let name = lodestone_data::block_states::block_type_name(state_id)?;
    COMMAND_BLOCKS
        .iter()
        .find(|(id, _)| *id == name)
        .map(|(_, mode)| *mode)
}

/// Whether `state_id`'s `conditional` property is set —
/// `CommandBlock.CONDITIONAL`.
///
/// Absent property reads `false`, matching
/// `CommandBlockEntity.isConditional()`'s own fallback for a block that is not
/// a `CommandBlock` (`CommandBlockEntity.java:155-158`).
#[must_use]
fn conditional_for_state(state_id: u32) -> bool {
    lodestone_data::block_states::properties(state_id)
        .into_iter()
        .flatten()
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
/// state and raw block-entity NBT.
///
/// Returns `None` only when `state_id` is not a command block — the one case
/// where opening the screen would be wrong. Every *data* problem below that
/// (no NBT at all, a malformed compound, a missing field, a wrongly-typed
/// field) degrades to vanilla's own default for that field.
#[must_use]
pub fn command_block_open(pos: BlockPos, state_id: u32, nbt: &Nbt) -> Option<CommandBlockOpen> {
    let mode = mode_for_state(state_id)?;
    let conditional = conditional_for_state(state_id);

    // `Nbt::End` is what `BlockEntity::nbt` holds for a block the server sent
    // no data for, which is the common case for one the player just placed.
    // That is not an error: vanilla opens an empty editor.
    let fields: &[(String, Nbt)] = match nbt {
        Nbt::Compound(fields) => fields,
        _ => &[],
    };

    // `BaseCommandBlock.load` defaults: `Command` to `""`, `TrackOutput` to
    // **`true`** (`BaseCommandBlock.java:62-65`) — note the asymmetry, it is
    // the one field here whose default is not `false`/empty.
    let command = find(fields, "Command")
        .and_then(as_string)
        .unwrap_or_default()
        .to_owned();
    let track_output = find(fields, "TrackOutput").and_then(as_bool).unwrap_or(true);
    let automatic = find(fields, "auto").and_then(as_bool).unwrap_or(false);

    // `LastOutput` is a chat `Component`, and vanilla only shows it when
    // tracking is on (`AbstractCommandBlockEditScreen.java:58` sets the
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

    /// A state id that is definitely not a command block, resolved from the
    /// real table rather than guessed — `0` happens to be air, but asserting
    /// that would be asserting a table detail this test does not care about.
    fn non_command_block_state() -> u32 {
        (0u32..4096)
            .find(|id| {
                lodestone_data::block_states::block_type_name(*id)
                    .is_some_and(|n| n == "minecraft:stone")
            })
            .expect("the block-state table must contain stone")
    }

    fn command_block_state() -> Option<u32> {
        (0u32..40_000).find(|id| {
            lodestone_data::block_states::block_type_name(*id)
                .is_some_and(|n| n == "minecraft:command_block")
        })
    }

    /// **The mode comes from the block, not the NBT** — the trap this module's
    /// doc names. A reader that only looked at the payload would report every
    /// chain block as Redstone, which is a plausible-looking wrong answer.
    #[test]
    fn each_command_block_reports_its_own_mode_and_a_normal_block_reports_none() {
        for (name, expected) in COMMAND_BLOCKS {
            let Some(id) = (0u32..40_000).find(|id| {
                lodestone_data::block_states::block_type_name(*id).is_some_and(|n| n == name)
            }) else {
                // The table is data-driven; skip rather than fail if a block is
                // absent, but the assertion below stops that being vacuous for
                // the whole test.
                continue;
            };
            assert_eq!(
                mode_for_state(id),
                Some(expected),
                "{name} must report its own mode"
            );
        }
        assert_eq!(
            mode_for_state(non_command_block_state()),
            None,
            "stone is not a command block, and must not open the screen"
        );
    }

    /// **`TrackOutput` defaults to `true`, unlike every other flag here.**
    ///
    /// Predicts the exact value rather than asserting "some default": a reader
    /// that defaulted the whole struct to `false` would be wrong in one field
    /// only, and that is precisely the field an unaware port gets wrong.
    #[test]
    fn an_empty_payload_opens_with_vanillas_own_field_initialisers() {
        let Some(id) = command_block_state() else {
            return;
        };
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
        let Some(id) = command_block_state() else {
            return;
        };
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
        let Some(id) = command_block_state() else {
            return;
        };
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
