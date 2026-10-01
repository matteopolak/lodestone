#[cfg(feature = "differential-campaign")]
pub use lodestone_fuzz::campaign::generation::*;

#[cfg(not(feature = "differential-campaign"))]
#[path = "../../src/campaign/generation.rs"]
mod implementation;

#[cfg(not(feature = "differential-campaign"))]
pub use implementation::*;
