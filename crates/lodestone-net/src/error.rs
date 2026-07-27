//! Error types for the Lodestone networking layer.

/// Convenient result alias for networking operations.
pub type Result<T> = core::result::Result<T, NetError>;

/// Errors produced while framing, (de)compressing, or transporting packets.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// A wrapped protocol-level codec error from `lodestone-core`.
    #[error("protocol codec error: {0}")]
    Codec(#[from] lodestone_core::Error),

    /// A frame declared a length above vanilla's hard cap.
    #[error("packet length {len} exceeds maximum {max}")]
    PacketTooLarge {
        /// Declared frame length.
        len: usize,
        /// Maximum permitted frame length.
        max: usize,
    },

    /// A length VarInt used more bytes than permitted for a frame header.
    #[error("length varint exceeds {max} bytes")]
    LengthVarIntTooLong {
        /// Maximum permitted VarInt byte count.
        max: usize,
    },

    /// A compressed frame carried a non-zero uncompressed length below the threshold.
    #[error("badly compressed packet: uncompressed length {len} is below threshold {threshold}")]
    BadlyCompressed {
        /// Declared uncompressed length.
        len: usize,
        /// Active compression threshold.
        threshold: usize,
    },

    /// A frame declared a decompressed size above the safety cap.
    #[error("decompressed size {len} exceeds maximum {max}")]
    DecompressedTooLarge {
        /// Declared decompressed length.
        len: usize,
        /// Maximum permitted decompressed length.
        max: usize,
    },

    /// Decompression produced a different number of bytes than declared.
    #[error("decompressed size mismatch: expected {expected}, got {actual}")]
    DecompressedLenMismatch {
        /// Declared decompressed length.
        expected: usize,
        /// Actual decompressed length.
        actual: usize,
    },

    /// A frame was structurally invalid (for example, zero length).
    #[error("malformed frame: {0}")]
    MalformedFrame(&'static str),

    /// The peer closed the connection in the middle of a frame.
    #[error("connection closed mid-frame ({0} bytes buffered)")]
    UnexpectedClose(usize),

    /// A shared secret was not the required 16-byte AES-128 key length.
    #[error("shared secret must be 16 bytes, got {len}")]
    BadSharedSecret {
        /// Actual secret length supplied.
        len: usize,
    },

    /// Encryption was enabled twice on one connection.
    #[error("encryption is already enabled on this connection")]
    EncryptionAlreadyEnabled,

    /// An RSA key-parse or encryption failure during the handshake.
    #[error("rsa error: {0}")]
    Rsa(String),

    /// A zlib (de)compression failure.
    #[error("zlib error: {0}")]
    Zlib(std::io::Error),

    /// An underlying transport I/O error.
    #[error("io error: {0}")]
    Io(std::io::Error),

    /// A network operation did not complete within its timeout.
    #[error("{operation} timed out after {seconds}s")]
    Timeout {
        /// The operation that timed out (e.g. `"connect"`).
        operation: &'static str,
        /// The elapsed timeout budget, in seconds.
        seconds: u64,
    },

    /// A DNS lookup (SRV record or name resolution) failed.
    #[error("dns resolution failed for {name}: {reason}")]
    Dns {
        /// The name being resolved.
        name: String,
        /// The underlying resolver error, rendered as text.
        reason: String,
    },
}

impl From<std::io::Error> for NetError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
