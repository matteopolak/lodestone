//! `java.util.Random`, reproduced exactly.
//!
//! Vanilla particles draw every one of their constants — lifetime, initial
//! velocity, colour jitter, sprite choice — from vanilla's own random-source
//! factory, which wraps the same 48-bit LCG as
//! `java.util.Random`. Reproducing it means a seeded engine replays a byte-exact
//! particle burst, which is what makes the parity tests in this crate able to
//! assert concrete numbers instead of ranges.
//!
//! # This is a re-export, not a copy
//!
//! [`JavaRandom`] used to be an independent implementation of the same LCG
//! that `lodestone-shell`'s enchanting-table book, `lodestone-render`'s
//! lightning bolt and `lodestone-audio`'s sound-variant selection each also
//! carried — five copies of the identical algorithm across the workspace. They
//! are now one: `lodestone-javarandom`, which every call site here uses
//! directly (`next_f32`/`next_f64`/`next_i32_bound`/`next_bool`, in place of
//! this module's old `next_float`/`next_double`/`next_int_bound`/`next_bool`
//! names). See that crate's docs for why `lodestone-worldgen-core`'s
//! legacy random-source implementation is the one deliberate holdout from the consolidation.
//!
//! # Parity is *not* required here, and that is worth stating
//!
//! Unlike physics or packets, particle randomness is never compared against a
//! server: particles are client-side decoration and no observer can tell our
//! stream from vanilla's. The reason to be exact anyway is **testability** — an
//! LCG with published constants lets a test's expected values originate outside
//! the code under test, which is the standing requirement in this project.

pub use lodestone_javarandom::JavaRandom;
