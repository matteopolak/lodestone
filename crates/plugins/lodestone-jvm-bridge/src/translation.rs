//! Value-only requests and answers crossing the JVM adapter boundary.
//!
//! This module deliberately contains no JNI object, connection, entity, world,
//! or worker state.  The adapter and its host use these copied values as the
//! translation layer between Java-facing calls and bounded host ports.

use jni::sys::jint;

use super::AdapterError;

/// A block query in the host's primary world, in absolute block coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockStateQuery {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// The largest number of absolute block positions one batch request may carry.
pub const MAX_BLOCK_STATE_BATCH_POSITIONS: usize = 4096;

/// The largest number of replacements one batch request may carry.
pub const MAX_BLOCK_STATE_BATCH_WRITES: usize = 64;

/// Explicit observer-notification policy for a resident block replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockUpdateFlags(u8);

impl BlockUpdateFlags {
    /// Apply the resident replacement without scheduling a Java observer callback.
    pub const NONE: Self = Self(0);
    /// Queue the established resident block-change callback after a successful replacement.
    pub const NOTIFY_RESIDENT_LISTENERS: Self = Self(1);

    pub(super) fn from_jint(flags: jint) -> Result<Self, AdapterError> {
        let flags = u32::try_from(flags).map_err(|_| {
            AdapterError::new("setBlockStateIdsWithFlags requires non-negative update flags")
        })?;
        match flags {
            0 => Ok(Self::NONE),
            1 => Ok(Self::NOTIFY_RESIDENT_LISTENERS),
            _ => Err(AdapterError::new(format!(
                "setBlockStateIdsWithFlags does not support update flags 0x{flags:x}; supported bits: 0x01 notify resident listeners"
            ))),
        }
    }

    /// Whether a successful host write must queue the existing listener callback.
    #[must_use]
    pub const fn notifies_resident_listeners(self) -> bool {
        self.0 & Self::NOTIFY_RESIDENT_LISTENERS.0 != 0
    }
}

/// Several resident block-state reads carried by one bounded port request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockStateBatchQuery {
    pub positions: Vec<BlockStateQuery>,
}

/// Several resident block-state replacements carried by one bounded request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockStateBatchWrite {
    pub writes: Vec<BlockStateWrite>,
    pub update_flags: BlockUpdateFlags,
}

/// A host must distinguish an unavailable position from a valid air state.
pub type BlockStateAnswer = Result<u32, String>;

/// Host answer for one ordered resident block-state batch.
pub type BlockStateBatchAnswer = Result<Vec<u32>, String>;

/// Host answer for one atomically preflighted resident block-state batch.
pub type BlockStateBatchWriteAnswer = Result<(), String>;

/// A requested replacement of one already-resident primary-world block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockStateWrite {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub state_id: u32,
}

/// A failed write must not be reported as a successful no-op.
pub type BlockStateWriteAnswer = Result<(), String>;

/// The value-only identity of the player associated with one host-confirmed callback.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PlayerIdentity {
    uuid: [u8; 16],
    name: String,
}

impl PlayerIdentity {
    /// Creates a player identity from its stable profile bytes and name.
    #[must_use]
    pub fn new(uuid: [u8; 16], name: impl Into<String>) -> Self {
        Self {
            uuid,
            name: name.into(),
        }
    }

    /// The stable profile bytes used to distinguish reconnects and players with the same name.
    #[must_use]
    pub const fn uuid(&self) -> [u8; 16] {
        self.uuid
    }

    /// The host-authored display name exposed by the narrow fixture query.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A live-position query keyed by copied account identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerSnapshotQuery {
    pub uuid: [u8; 16],
}

/// A copied player position returned by the host.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerSnapshot {
    pub entity_id: i32,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub game_mode: PlayerGameMode,
    pub experience_level: i32,
    pub experience_points: i32,
}

/// The closed game-mode vocabulary carried across the host port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerGameMode {
    Survival,
    Creative,
    Adventure,
    Spectator,
}

/// A disconnected player must remain distinct from a valid origin position.
pub type PlayerSnapshotAnswer = Result<PlayerSnapshot, String>;

/// A bounded request to relocate one connected player through the host's authoritative path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerTeleportRequest {
    pub uuid: [u8; 16],
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// A host must distinguish a queued teleport from a disconnected player.
pub type PlayerTeleportAnswer = Result<(), String>;

/// One native-inventory slot requested for a generation-checked player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerInventorySlotQuery {
    pub uuid: [u8; 16],
    pub native_slot: i32,
}

/// The deliberately narrow read-only item projection returned by the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerInventorySlot {
    pub item_key: String,
    pub count: u32,
    pub unmodeled: bool,
}

/// Empty and unavailable slots remain different.
pub type PlayerInventorySlotAnswer = Result<Option<PlayerInventorySlot>, String>;

/// A host must distinguish an inactive game tick from a valid count.
pub type ServerTickAnswer = Result<u64, String>;

/// Parses the canonical textual UUID representation used by Java lookups.
pub(super) fn parse_uuid_string(value: &str, operation: &str) -> Result<[u8; 16], AdapterError> {
    let bytes = value.as_bytes();
    if bytes.len() != 36 || ![8, 13, 18, 23].into_iter().all(|index| bytes[index] == b'-') {
        return Err(AdapterError::new(format!(
            "{operation}: invalid UUID {value:?} (expected 36-character form)",
        )));
    }
    let mut uuid = [0; 16];
    let mut output = 0;
    let mut index = 0;
    while index < bytes.len() {
        if matches!(index, 8 | 13 | 18 | 23) {
            index += 1;
            continue;
        }
        let high = hex_digit(bytes[index]).ok_or_else(|| {
            AdapterError::new(format!(
                "{operation}: invalid UUID {value:?} (non-hex digit)",
            ))
        })?;
        let low = hex_digit(bytes[index + 1]).ok_or_else(|| {
            AdapterError::new(format!(
                "{operation}: invalid UUID {value:?} (non-hex digit)",
            ))
        })?;
        uuid[output] = (high << 4) | low;
        output += 1;
        index += 2;
    }
    Ok(uuid)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Formats copied UUID bytes in the canonical textual representation.
pub(super) fn canonical_uuid_string(uuid: [u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(36);
    for (index, byte) in uuid.into_iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        text.push(HEX[usize::from(byte >> 4)] as char);
        text.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    text
}
