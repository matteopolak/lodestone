//! Errors surfaced by the audio engine.

/// Any failure inside `lodestone-audio`.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// The Ogg Vorbis bitstream could not be decoded.
    #[error("ogg/vorbis decode failed: {0}")]
    Decode(String),

    /// A decoded stream had zero channels or an otherwise unusable format.
    #[error("unsupported audio format: {0}")]
    Format(String),

    /// The native audio device could not be opened or configured.
    #[error("audio device error: {0}")]
    Device(String),
}
