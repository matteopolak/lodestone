//! Canonicalisation of pre-Flattening block data into the project's single
//! canonical block-state space (26.2, `lodestone_data::block_states`).
//!
//! # What it is
//!
//! Two layers, both extracted here from `lodestone-v340` so that *every*
//! pre-1.13 protocol family can share one copy
//! instead of each carrying its own:
//!
//! - [`flattening`] — the `(old_block_id, meta)` → modern-block-state table,
//!   a reflective dump of the real 1.13.2 server jar's own `DataFixerUpper`
//!   flattening fix. Provenance is the jar, not this repo; see
//!   `tests/flattening.rs` for the generate-or-assert drift guard and
//!   `oracle-java/FlatteningOracle.java` for the dump program.
//! - [`canonical`] — the bridge from that table's (18w-snapshot-era) names
//!   and properties to a concrete canonical 26.2 block-state id, which is the
//!   id space `lodestone_world::PalettedContainer` consumers (the mesher's
//!   atlas, collision) are actually built from.
//!
//! # Why a shared crate rather than a copy per family
//!
//! Every version below 1.13 speaks `id:meta`, and the dumped table is the
//! 1.13.2 DataFixer's — it upgrades *1.12.2-space* ids, and older versions'
//! id space is a strict subset (ids were only ever added), so one table
//! serves 1.7.10 through 1.12.2. The per-version difference is which slots
//! are populated, which [`flattening::LegacyBlockState::NoTableEntry`]
//! already expresses. The alternative — the four pre-1.13 families each
//! carrying their own copy of a 9k-line generated table — drifts
//! independently and multiplies the regeneration cost by four.
//!
//! This does not weaken per-family deletability, the reason the copy was
//! originally denied to `v47`: deletability applies to *families*, and
//! deleting one is still its folder plus its dependency line and feature line
//! in `lodestone-registry` (`cargo xtask check-deletable <vNNN>` verifies
//! exactly that). Shared game data living in a shared crate has precedent in
//! `lodestone-data`.
//!
//! # How to change it
//!
//! The table is **generated**. Never hand-edit `src/generated/flattening.rs`
//! — re-dump from the jar and regenerate; `tests/flattening.rs` documents
//! both steps and fails loudly if the committed file drifts from the
//! committed dump. The hand-written parts are [`canonical`]'s rename and
//! property bridges, each entry of which carries its own justification.
//!
//! # Dependencies
//!
//! `lodestone-data` only, for the canonical 26.2 block-state registry. This
//! crate names no protocol family and must not start.

#![forbid(unsafe_code)]

/// Generated `id:meta` (pre-Flattening) → modern block-state table, derived
/// from the real 1.13.2 server jar's own `DataFixerUpper` flattening fix
/// rather than from any community dataset (`minecraft-data` has no such table
/// at all). See `tests/flattening.rs` for the generator and
/// `docs/protocol-340-flattening-table.md` for the full ambiguous-case
/// enumeration.
#[path = "generated/flattening.rs"]
pub(crate) mod generated_flattening;

pub mod canonical;
pub mod flattening;
