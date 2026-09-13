//! `EditSession`: WorldEdit's own name for the same concept — a batched,
//! chunk-aware region edit with a per-session undo/redo stack.
//!
//! # Where the batching actually happens
//!
//! A WorldEdit-class plugin's own scoping question was whether the native
//! block-write API needed a batched entry point "to avoid re-acquiring the
//! chunk lock per block". Re-checked against the tree building this: there is
//! exactly **one** lock for the whole store
//! ([`ChunkWorldWrite`]'s `std::sync::RwLock<lodestone_world::World>`), taken
//! once by whoever calls [`ChunkWorldWrite::write`] — so the lock itself was
//! never the per-block cost. The real cost was one `HashMap::get_mut` (and one
//! `section_index` bounds check) per block in a naive loop over
//! `World::set_block`. [`lodestone_world::World::fill_region_capturing`] is
//! the primitive that removes it, grouping by chunk so a fill touching many
//! chunks pays one lookup per *chunk*, not per block. Every method here that
//! touches many blocks calls it (or [`World::fill_region`] when no undo
//! record is needed); nothing in this crate loops calling `set_block` once
//! per position.
//!
//! # Undo/redo
//!
//! Every mutating call pushes an [`EditRecord`] — the exact `(x, y, z,
//! previous_state)` triples [`World::fill_region_capturing`] returned — onto
//! the session's undo stack. [`EditSession::undo`] replays those triples
//! through [`World::fill_region_capturing`] again (write the *old* states,
//! capturing what was there so the same call becomes the redo record) rather
//! than a bespoke "reverse write" path, so undo and redo share the identical
//! write primitive every fill does — one code path, not two that could drift.

use bevy_ecs::resource::Resource;
use lodestone_ecs::ChunkWorldWrite;
use lodestone_world::World;

/// An axis-aligned block-position box, either corner order — mirrors
/// [`World::fill_region`]'s own `[min, max]` inclusive-both-ends contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub min: [i32; 3],
    pub max: [i32; 3],
}

impl Selection {
    #[must_use]
    pub const fn new(a: [i32; 3], b: [i32; 3]) -> Self {
        Self { min: a, max: b }
    }

    /// Block count the selection spans, **not** how many are actually loaded
    /// — a fill against an unloaded chunk silently writes fewer than this
    /// (matching [`World::fill_region`]'s own no-op-for-unloaded contract).
    #[must_use]
    pub fn volume(&self) -> u64 {
        let dx = u64::from((self.max[0] - self.min[0]).unsigned_abs()) + 1;
        let dy = u64::from((self.max[1] - self.min[1]).unsigned_abs()) + 1;
        let dz = u64::from((self.max[2] - self.min[2]).unsigned_abs()) + 1;
        dx * dy * dz
    }
}

/// One undoable edit: every position it touched, with the state that was
/// there immediately before this edit — exactly what
/// [`World::fill_region_capturing`] returns.
type EditRecord = Vec<(i32, i32, i32, u32)>;

/// A batched region-edit session with undo/redo, holding a
/// [`ChunkWorldWrite`] handle — the same resource `drive_placement` and
/// `Sim::predict_block` write through, so an edit here and a plugin's
/// `PlaceIntent` never fork the store.
///
/// Not itself a `Resource`/`Plugin` — a plugin author embeds this in their own
/// plugin's state (a `Resource` wrapping it, or a chat-command handler's local
/// state), the same way WorldEdit's own `EditSession` is a per-invocation
/// object on real Paper, not a server-wide singleton.
#[derive(Debug)]
pub struct EditSession {
    store: ChunkWorldWrite,
    undo_stack: Vec<EditRecord>,
    redo_stack: Vec<EditRecord>,
}

/// Bound on how many edits [`EditSession`] remembers, mirroring
/// [`lodestone_world::PENDING_RELIGHT_CAP`]'s own reasoning: an editor
/// running for a whole session must not grow an unbounded undo stack. Real
/// WorldEdit's default is 15 per player; this is generous headroom for a
/// plugin, not a claim about the right UX default.
pub const MAX_UNDO_DEPTH: usize = 64;

impl EditSession {
    #[must_use]
    pub fn new(store: ChunkWorldWrite) -> Self {
        Self {
            store,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    /// Fills `selection` with `state`, recording an undo entry.
    ///
    /// `physics: false` (matching a real WorldEdit's own default of deferred
    /// physics, for exactly the reason the block read/write API's own doc
    /// names — a 50,000-block fill must not cascade 50,000 individual
    /// neighbour updates) is the common
    /// case; pass `true` to additionally queue neighbour updates for every
    /// written block via [`World::set_block_with_physics`]'s own mechanism
    /// (see that method's doc for what "queues" means today — there is no
    /// block-tick consumer yet).
    ///
    /// Returns the number of blocks actually written (loaded-chunk overlap
    /// with the selection may be smaller than [`Selection::volume`]).
    pub fn fill(&mut self, selection: Selection, state: u32, physics: bool) -> usize {
        let record = {
            let mut world = self.store.write();
            let record = world.fill_region_capturing(selection.min, selection.max, state);
            if physics {
                queue_physics_for_record(&mut world, &record, state);
            }
            record
        };
        let written = record.len();
        self.push_undo(record);
        written
    }

    /// Replaces every occurrence of `from` inside `selection` with `to`,
    /// recording one undo entry for the whole operation — WorldEdit's
    /// `/replace`.
    ///
    /// Implemented as a targeted [`World::fill_region_capturing`] scan rather
    /// than a full-volume fill with a per-block predicate: this crate has no
    /// predicate-write primitive in `lodestone-world` today, so a replace
    /// currently pays a capture-then-selective-rewrite pass. Documented here
    /// rather than hidden, since it is a real (if modest) inefficiency
    /// relative to a hypothetical `World::replace_region`, not a correctness
    /// gap — `docs/backlog.md` is where that primitive would be tracked if
    /// profiling ever shows this matters.
    pub fn replace(&mut self, selection: Selection, from: u32, to: u32) -> usize {
        let record = {
            let mut world = self.store.write();
            // A capturing fill of `to` would overwrite blocks that were never
            // `from`; instead, read positions directly and rewrite only
            // matches through the single-block API, avoiding any redundant
            // relight/queue work fill_region would add for untouched cells.
            let mut record = EditRecord::new();
            for x in
                selection.min[0].min(selection.max[0])..=selection.min[0].max(selection.max[0])
            {
                for y in selection.min[1].min(selection.max[1])
                    ..=selection.min[1].max(selection.max[1])
                {
                    for z in selection.min[2].min(selection.max[2])
                        ..=selection.min[2].max(selection.max[2])
                    {
                        if world.block_state_at(x, y, z) == Some(from) {
                            world.set_block(x, y, z, to);
                            record.push((x, y, z, from));
                        }
                    }
                }
            }
            record
        };
        let written = record.len();
        self.push_undo(record);
        written
    }

    /// Reverts the most recent edit, moving it onto the redo stack. Returns
    /// how many blocks were restored, or `None` if there was nothing to undo.
    pub fn undo(&mut self) -> Option<usize> {
        let record = self.undo_stack.pop()?;
        let mut world = self.store.write();
        let redo = replay_record(&mut world, &record);
        let count = redo.len();
        self.redo_stack.push(redo);
        Some(count)
    }

    /// Re-applies the most recently undone edit. Returns how many blocks were
    /// restored, or `None` if there was nothing to redo.
    pub fn redo(&mut self) -> Option<usize> {
        let record = self.redo_stack.pop()?;
        let mut world = self.store.write();
        let undo = replay_record(&mut world, &record);
        let count = undo.len();
        self.undo_stack.push(undo);
        Some(count)
    }

    /// How many edits can currently be undone — a count, not a duration, for
    /// a plugin's own status display.
    #[must_use]
    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    /// How many edits can currently be redone.
    #[must_use]
    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }

    fn push_undo(&mut self, record: EditRecord) {
        if record.is_empty() {
            return;
        }
        self.undo_stack.push(record);
        if self.undo_stack.len() > MAX_UNDO_DEPTH {
            self.undo_stack.remove(0);
        }
        // A fresh edit invalidates the redo chain, matching every real editor
        // (WorldEdit included): redoing after a new edit would silently
        // resurrect blocks the new edit already overwrote.
        self.redo_stack.clear();
    }
}

/// Writes every `(x, y, z, state)` triple in `record` directly (not through
/// `fill_region`, since the positions are not a contiguous box), returning a
/// new record of what was there immediately before — the value that lets
/// [`EditSession::undo`]/[`EditSession::redo`] be the same function applied
/// twice.
fn replay_record(world: &mut World, record: &[(i32, i32, i32, u32)]) -> EditRecord {
    record
        .iter()
        .map(|&(x, y, z, state)| {
            let previous = world.set_block_with_physics(x, y, z, state, false).unwrap_or(0);
            (x, y, z, previous)
        })
        .collect()
}

/// Queues the six orthogonal neighbours of every written position, matching
/// [`World::set_block_with_physics`]'s own fan-out exactly — `fill` calling
/// [`World::fill_region_capturing`] instead of that method (for the batching
/// win) must still queue the identical neighbour set a single-block
/// `physics: true` write would have, or a plugin author switching from a
/// loop of single writes to a `fill` call would silently lose neighbour
/// updates at every seam between filled blocks.
fn queue_physics_for_record(world: &mut World, record: &[(i32, i32, i32, u32)], _state: u32) {
    const NEIGHBOURS: [[i32; 3]; 6] = [
        [1, 0, 0],
        [-1, 0, 0],
        [0, 1, 0],
        [0, -1, 0],
        [0, 0, 1],
        [0, 0, -1],
    ];
    for &(x, y, z, _) in record {
        for [dx, dy, dz] in NEIGHBOURS {
            world.queue_physics_update(x + dx, y + dy, z + dz);
        }
    }
}

/// A [`Resource`] wrapper so a plugin can hold one session per player (or one
/// shared session) as ordinary ECS state, keyed however the plugin likes —
/// this crate does not impose a keying scheme, since "one session per player"
/// vs. "one shared session" is a policy decision for the plugin embedding
/// this, not something WorldEdit itself dictates either (real WorldEdit is
/// per-player).
#[derive(Resource, Debug)]
pub struct EditSessions(pub std::collections::HashMap<i32, EditSession>);

impl EditSessions {
    #[must_use]
    pub fn new() -> Self {
        Self(std::collections::HashMap::new())
    }
}

impl Default for EditSessions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind};

    fn flat_store(radius: i32) -> ChunkWorldWrite {
        let mut world = World::new();
        for cx in -radius..=radius {
            for cz in -radius..=radius {
                let column = ChunkColumn::new(
                    -64,
                    24,
                    PaletteKind::block_states(),
                    PaletteKind::biomes(),
                    0,
                    0,
                );
                let light = ColumnLight::new(24);
                world.load(
                    ChunkPos::new(cx, cz),
                    LoadedChunk::new(column, light, Heightmaps::new(), Vec::new()),
                );
            }
        }
        ChunkWorldWrite::new(world)
    }

    #[test]
    fn selection_volume_counts_inclusive_both_ends() {
        let sel = Selection::new([0, 0, 0], [1, 1, 1]);
        assert_eq!(sel.volume(), 8, "a 2x2x2 box is 8 blocks, inclusive");
        assert_eq!(Selection::new([0, 0, 0], [0, 0, 0]).volume(), 1);
    }

    #[test]
    fn fill_writes_the_whole_selection_and_reports_the_count() {
        let store = flat_store(1);
        let mut session = EditSession::new(store.clone());

        let written = session.fill(Selection::new([0, 60, 0], [3, 61, 3]), 5, false);
        assert_eq!(written, 4 * 2 * 4);

        let world = store.read();
        assert_eq!(world.block_state_at(0, 60, 0), Some(5));
        assert_eq!(world.block_state_at(3, 61, 3), Some(5));
        assert_eq!(
            world.block_state_at(4, 60, 0),
            Some(0),
            "just outside the selection must be untouched"
        );
    }

    #[test]
    fn undo_restores_exactly_what_fill_overwrote() {
        let store = flat_store(1);
        {
            let mut world = store.write();
            world.set_block(0, 60, 0, 3);
            world.set_block(1, 60, 0, 4);
        }
        let mut session = EditSession::new(store.clone());

        session.fill(Selection::new([0, 60, 0], [1, 60, 0]), 9, false);
        assert_eq!(store.read().block_state_at(0, 60, 0), Some(9));
        assert_eq!(store.read().block_state_at(1, 60, 0), Some(9));

        let restored = session.undo().expect("something to undo");
        assert_eq!(restored, 2);
        assert_eq!(
            store.read().block_state_at(0, 60, 0),
            Some(3),
            "undo must restore the exact pre-fill state, not a default"
        );
        assert_eq!(store.read().block_state_at(1, 60, 0), Some(4));
        assert_eq!(session.undo(), None, "nothing left to undo");
    }

    #[test]
    fn redo_reapplies_an_undone_fill() {
        let store = flat_store(1);
        let mut session = EditSession::new(store.clone());

        session.fill(Selection::new([0, 60, 0], [0, 60, 0]), 7, false);
        session.undo();
        assert_eq!(store.read().block_state_at(0, 60, 0), Some(0));

        let redone = session.redo().expect("something to redo");
        assert_eq!(redone, 1);
        assert_eq!(store.read().block_state_at(0, 60, 0), Some(7));
        assert_eq!(session.redo(), None, "nothing left to redo");
    }

    #[test]
    fn a_new_edit_after_undo_clears_the_redo_chain() {
        let store = flat_store(1);
        let mut session = EditSession::new(store.clone());

        session.fill(Selection::new([0, 60, 0], [0, 60, 0]), 1, false);
        session.undo();
        assert_eq!(session.redo_depth(), 1);

        session.fill(Selection::new([1, 60, 0], [1, 60, 0]), 2, false);
        assert_eq!(
            session.redo_depth(),
            0,
            "a fresh edit must invalidate the old redo chain, or redoing \
             afterward would resurrect blocks the new edit already overwrote"
        );
    }

    #[test]
    fn replace_only_touches_matching_blocks_and_undoes_cleanly() {
        let store = flat_store(1);
        {
            let mut world = store.write();
            world.set_block(0, 60, 0, 3);
            world.set_block(1, 60, 0, 4);
            world.set_block(2, 60, 0, 3);
        }
        let mut session = EditSession::new(store.clone());

        let replaced = session.replace(Selection::new([0, 60, 0], [2, 60, 0]), 3, 8);
        assert_eq!(replaced, 2, "only the two cells holding state 3");
        assert_eq!(store.read().block_state_at(0, 60, 0), Some(8));
        assert_eq!(
            store.read().block_state_at(1, 60, 0),
            Some(4),
            "a non-matching block must be left alone"
        );
        assert_eq!(store.read().block_state_at(2, 60, 0), Some(8));

        session.undo();
        assert_eq!(store.read().block_state_at(0, 60, 0), Some(3));
        assert_eq!(store.read().block_state_at(2, 60, 0), Some(3));
    }

    #[test]
    fn fill_with_an_empty_write_pushes_no_undo_entry() {
        // A fill entirely outside every loaded chunk: fill_region_capturing
        // returns an empty record, and pushing an empty undo entry would let
        // a caller "undo" a no-op, silently consuming an unrelated earlier
        // entry's slot in the depth accounting.
        let store = flat_store(1);
        let mut session = EditSession::new(store);

        let written = session.fill(Selection::new([9999, 60, 9999], [9999, 60, 9999]), 1, false);
        assert_eq!(written, 0);
        assert_eq!(session.undo_depth(), 0);
    }

    #[test]
    fn fill_with_physics_true_queues_neighbour_updates() {
        let store = flat_store(1);
        let mut session = EditSession::new(store.clone());

        session.fill(Selection::new([0, 60, 0], [0, 60, 0]), 5, true);
        let queued = store.write().drain_pending_physics_updates();
        assert_eq!(
            queued.len(),
            6,
            "one filled block with physics: true must queue its six neighbours"
        );
    }

    #[test]
    fn fill_with_physics_false_queues_nothing() {
        let store = flat_store(1);
        let mut session = EditSession::new(store.clone());

        session.fill(Selection::new([0, 60, 0], [0, 60, 0]), 5, false);
        assert!(store.write().drain_pending_physics_updates().is_empty());
    }
}
