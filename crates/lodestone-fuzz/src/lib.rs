//! Fuzz/property-testing harness for lodestone's wire decoders.
//!
//! ## What it is
//!
//! Everything a wire decoder receives ultimately comes from the network, so
//! "does this decoder do something safe on bytes nobody chose for a reason"
//! is a property that needs no expected value at all — unlike
//! `decode(encode(x)) == x`, which CLAUDE.md's own record shows can be
//! satisfied by two symmetric misunderstandings (hermetic chunk fixtures that
//! passed for months, then 49 × "unexpected end of input" against a real
//! server). This crate is the harness that checks the properties which *are*
//! true independent of whether we understand the protocol correctly: no
//! panic on arbitrary bytes, a truncated valid packet errors cleanly, and a
//! length prefix cannot force an allocation disconnected from the bytes
//! actually available.
//!
//! See `docs/fuzz-harness.md` for the full writeup: which decoders are
//! covered, the case/size caps, the corpus sources (and which of those are
//! self-encoded and therefore weaker evidence per CLAUDE.md), and the one
//! real bug this harness found on first run.
//!
//! This module holds only shared plumbing: a no-op [`WorldSink`], the
//! panic-catching wrapper every property test calls through, and the
//! per-family/per-state packet-id tables used to pick realistic packet ids
//! instead of pure `any::<i32>()` noise.

use lodestone_core::Nbt;
use lodestone_model::{ConnectionState, Directive, VersionAdapter};
use lodestone_world::{BiomePatch, BlockEntitySync, ChunkPos, ColumnPatch, LightPatch, LoadedChunk, WorldSink};

pub mod differential;

/// A [`WorldSink`] that discards every terrain call. Fuzz targets only care
/// whether decoding panics, not what it decoded to, so every write is a
/// no-op — mirrors the `NullSink` pattern already used by
/// `crates/versions/26.2/tests/entity_encoders.rs` and friends.
#[derive(Debug, Default)]
pub struct NullSink;

impl WorldSink for NullSink {
    fn load(&mut self, _pos: ChunkPos, _chunk: LoadedChunk) {}
    fn merge(&mut self, _pos: ChunkPos, _patch: ColumnPatch) {}
    fn set_block(&mut self, _x: i32, _y: i32, _z: i32, _state: u32) {}
    fn set_blocks(
        &mut self,
        _section_x: i32,
        _section_y: i32,
        _section_z: i32,
        _blocks: &[(u8, u8, u8, u32)],
    ) {
    }
    fn merge_light(&mut self, _pos: ChunkPos, _patch: LightPatch) {}
    fn merge_biomes(&mut self, _pos: ChunkPos, _patch: BiomePatch) {}
    fn unload(&mut self, _pos: ChunkPos) {}
    fn set_block_entity(&mut self, _x: i32, _y: i32, _z: i32, _type_id: u32, _nbt: Nbt) {}
    fn sync_block_entity(
        &mut self,
        _x: i32,
        _y: i32,
        _z: i32,
        _block_entity_type: Option<u32>,
    ) -> BlockEntitySync {
        BlockEntitySync::ChunkAbsent
    }
}

/// The client protocol families compiled into this build, each behind its own
/// workspace member per `CLAUDE.md`. `v1-14` speaks protocol 754 (1.16.5) — the
/// folder name is not the protocol number, so this enum never derives a protocol
/// from a variant name; adapters answer that themselves via
/// `VersionAdapter::supports`.
///
/// Every variant is behind the Cargo feature that compiles its family in. All
/// four are on by default, so the ordinary build has all four — the gating exists
/// so that deleting a family's folder is a matter of removing its lines from
/// `Cargo.toml`, which is the deletability invariant `xtask check-isolation`
/// enforces. Read the manifest's `[features]` comment before turning any of them
/// off: a family-less build makes every sweep below vacuous, and
/// `families_are_compiled_in` exists to catch that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    #[cfg(feature = "v1-8")]
    V47,
    #[cfg(feature = "v1-9")]
    V340,
    #[cfg(feature = "v1-14")]
    V735,
    #[cfg(feature = "v26-2")]
    V770,
}

impl Family {
    /// Every family compiled into this build.
    ///
    /// A slice rather than the `[Family; 4]` it used to be: the length is now a
    /// function of the enabled features, so a fixed-size array could not express
    /// it. Callers iterate with `for &family in Family::ALL`.
    pub const ALL: &'static [Family] = &[
        #[cfg(feature = "v1-8")]
        Family::V47,
        #[cfg(feature = "v1-9")]
        Family::V340,
        #[cfg(feature = "v1-14")]
        Family::V735,
        #[cfg(feature = "v26-2")]
        Family::V770,
    ];

    pub fn name(self) -> &'static str {
        match self {
            #[cfg(feature = "v1-8")]
            Family::V47 => "v1-8",
            #[cfg(feature = "v1-9")]
            Family::V340 => "v1-9",
            #[cfg(feature = "v1-14")]
            Family::V735 => "v1-14",
            #[cfg(feature = "v26-2")]
            Family::V770 => "v26-2",
        }
    }

    /// Builds a fresh boxed adapter. Fresh per call: `V770Adapter` carries
    /// interior-mutable per-connection state (chunk shape, batch tracking,
    /// movement send state), and reusing one across unrelated fuzz cases
    /// would let one case's state bleed into the next.
    pub fn adapter(self) -> Box<dyn VersionAdapter> {
        match self {
            #[cfg(feature = "v1-8")]
            Family::V47 => Box::new(lodestone_v1_8::V47Adapter::default()),
            #[cfg(feature = "v1-9")]
            Family::V340 => Box::new(lodestone_v1_9::V340Adapter::default()),
            #[cfg(feature = "v1-14")]
            Family::V735 => Box::new(lodestone_v1_14::V735Adapter::default()),
            #[cfg(feature = "v26-2")]
            Family::V770 => Box::new(lodestone_v26_2::V770Adapter::default()),
        }
    }

    /// Returns every clientbound `(name, packet_id)` this family declares for
    /// `state`, from the same generated `packet_ids` tables
    /// `VersionAdapter::handle_packet` callers use in production. Empty
    /// slices (e.g. no family declares `Status`-state clientbound packets
    /// beyond the two status ones) are a legitimate answer, not a bug.
    pub fn clientbound_entries(self, state: ConnectionState) -> &'static [(&'static str, i32)] {
        macro_rules! table {
            ($module:ident) => {
                match state {
                    ConnectionState::Handshaking => $module::packet_ids::handshaking::clientbound::ENTRIES,
                    ConnectionState::Status => $module::packet_ids::status::clientbound::ENTRIES,
                    ConnectionState::Login => $module::packet_ids::login::clientbound::ENTRIES,
                    ConnectionState::Configuration => $module::packet_ids::configuration::clientbound::ENTRIES,
                    ConnectionState::Play => $module::packet_ids::play::clientbound::ENTRIES,
                }
            };
        }
        match self {
            #[cfg(feature = "v1-8")]
            Family::V47 => table!(lodestone_v1_8),
            #[cfg(feature = "v1-9")]
            Family::V340 => table!(lodestone_v1_9),
            #[cfg(feature = "v1-14")]
            Family::V735 => table!(lodestone_v1_14),
            #[cfg(feature = "v26-2")]
            Family::V770 => table!(lodestone_v26_2),
        }
    }

    /// All five [`ConnectionState`] phases, for sweeping every state a
    /// family might see a decode call in.
    pub const STATES: [ConnectionState; 5] = [
        ConnectionState::Handshaking,
        ConnectionState::Status,
        ConnectionState::Login,
        ConnectionState::Configuration,
        ConnectionState::Play,
    ];
}

/// Runs `handle_packet` for `family` at `state` with `packet_id`/`payload`,
/// through a fresh adapter and a [`NullSink`]. Returns the `Result` the real
/// driver would see — callers decide what "clean" means (an `Err` is a
/// perfectly fine outcome for malformed input; a panic is not).
pub fn decode_clientbound(
    family: Family,
    state: ConnectionState,
    packet_id: i32,
    payload: &[u8],
) -> Result<Vec<Directive>, lodestone_model::AdapterError> {
    let adapter = family.adapter();
    let mut sink = NullSink;
    adapter.handle_packet(&mut sink, state, packet_id, payload)
}

/// Runs `f` under [`std::panic::catch_unwind`] and turns a panic into a
/// readable `Err(String)` instead of `Err(Box<dyn Any>)`. Every property test
/// in this crate calls decoders through this, never directly — a bare call
/// would abort the whole `cargo test` process on the first panic instead of
/// reporting a shrunk failing case.
///
/// This is the exact mechanism `tests/harness_control.rs` proves actually
/// detects a panic (see that file) — this function is not itself tested
/// there because it has no branch of its own to get wrong: it is a one-line
/// call to `catch_unwind`. What the control proves is that catching a panic
/// here reliably turns into a reported property failure rather than being
/// silently swallowed.
pub fn catch<R>(f: impl FnOnce() -> R + std::panic::UnwindSafe) -> Result<R, String> {
    std::panic::catch_unwind(f).map_err(|payload| {
        if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "panic with non-string payload".to_string()
        }
    })
}

/// Reads a `#`-commented hex-dump fixture in the same format
/// `crates/versions/26.2/tests/world_state.rs` uses for its captured-bytes
/// oracles: one or more whitespace-separated hex byte tokens per line, lines
/// starting with `#` ignored. Used to load `crates/versions/26.2/tests/fixtures/*.hex`
/// — captured server bytes, not anything our own encoder produced — as
/// fuzz-corpus seeds. See `docs/fuzz-harness.md` for which corpus entries
/// come from here versus from our own encoders.
pub fn read_hex_fixture(path: &std::path::Path) -> Vec<u8> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading fixture {}: {e}", path.display()));
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|tok| u8::from_str_radix(tok, 16).unwrap_or_else(|e| panic!("bad hex byte {tok:?} in {}: {e}", path.display())))
        .collect()
}

/// Absolute path to `crates/versions/26.2/tests/fixtures/<name>`, resolved
/// from this crate's own manifest dir so it does not depend on the caller's
/// working directory (`cargo test` from a subdirectory, an IDE runner, …).
pub fn v26_2_fixture_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../versions/26.2/tests/fixtures")
        .join(name)
}

/// Absolute path to this crate's own `tests/fixtures/<name>`, resolved the same
/// manifest-dir way as [`v26_2_fixture_path`].
///
/// These are *regression* inputs rather than corpus seeds: bytes a fuzz target
/// actually found a defect with, committed so the gate for that defect is
/// deterministic instead of "whatever proptest's RNG lands on this run".
/// Keeping the literal payload makes a discovered decoder failure reproducible
/// even when a filtered generator run does not select the same case. Each
/// fixture's header records where its bytes came from.
pub fn regression_fixture_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}
