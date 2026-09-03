//! Legacy (pre-1.19 signing) chat packets.
//!
//! [`ClientboundChat`] is shared by v1-8, v1-9 and v1-13 (protocols 47
//! through 404): 1.16 (v1-14, protocol 754) added a `sender: Uuid` field, so
//! it is declared `#[mc(protocols = "47..=404")]` and v1-14 keeps its own
//! three-field version. The upper bound moved 340 -> 404 when the 1.13 era
//! landed, with a real 1.13.2 join capture
//! (`crates/versions/1.13/tests/captures/join_1_13_2.txt`) decoding through
//! it -- a widening without one is the inheritance-by-range hazard the dedup
//! plan names.
//!
//! [`ServerboundChat`] and [`ServerboundArmAnimation`] are shared only from
//! 110 up: 1.8 capped the chat message at 100 characters (1.11+ raised it to
//! 256), and 1.8 has no separate arm-swing hand field at all (added with the
//! 1.9 off-hand). Their **upper** bounds differ, and the difference is the
//! point: `ServerboundArmAnimation` widened to 762 with the 1.19 era, but
//! `ServerboundChat` stops at 758 and must stay there. 1.19 replaced sending
//! a message with sending a signed, timestamped, salted body plus a last-seen
//! acknowledgement window; the string is still first, so a widened definition
//! would encode a prefix the server reads and then reject the connection for
//! the missing tail rather than fail here.

//!
//! The upper bound moved 758 -> 762 when the 1.19 era landed. `minecraft-data`
//! reports each widened packet's shape identical from 758 to 762 (named types
//! inlined, primitive aliases kept), and each additionally decodes or encodes
//! out of the committed real-join capture at
//! `crates/versions/1.19/tests/captures/join_1_19_4.txt` -- a widening without
//! one is the inheritance-by-range hazard the dedup plan names.

use lodestone_macros::{Decode, Encode, Packet};

/// Clientbound `chat` packet. Shared 47..=404 -- see the module docs.
///
/// Wire layout: string message (JSON), signed byte position (`0` chat, `1`
/// system, `2` action bar).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat", state = Play, bound = Client, protocols = "47..=404")]
pub struct ClientboundChat {
    /// JSON-encoded chat component.
    pub message: String,
    /// Chat slot: `0` chat, `1` system, `2` action bar.
    pub position: i8,
}

/// Serverbound `chat` packet. Shared only 340..=754 -- see the module docs
/// (1.8 capped the message at 100 characters, not 256).
///
/// Wire layout: a single string (max 256 chars).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat", state = Play, bound = Server, protocols = "110..=758")]
pub struct ServerboundChat {
    /// Message text (or `/command`), at most 256 characters (1.11+ raised this
    /// from the 100-character 1.8 limit).
    #[mc(max = 256)]
    pub message: String,
}

/// Serverbound `arm_animation` (swing arm) packet. Shared only 340..=754 --
/// see the module docs. Unlike 1.8 (protocol 47), where this packet is
/// empty, 1.9+ carries which hand swung as a VarInt (`0` = main, `1` =
/// off).
///
/// Wire layout: a single varint hand (`0` main, `1` off).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:arm_animation", state = Play, bound = Server, protocols = "110..=762")]
pub struct ServerboundArmAnimation {
    /// Hand that swung: `0` = main hand, `1` = off hand.
    #[mc(varint)]
    pub hand: i32,
}
