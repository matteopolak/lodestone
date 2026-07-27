//! Standalone hashing used by worldgen seeding.
//!
//! Vanilla derives some worldgen seeds by hashing strings: `XoroshiroRandomSource`
//! positional factories seed a 128-bit generator from the **MD5** of a resource
//! name, and the legacy positional factory uses Java's `String::hashCode`. Both
//! are reproduced here from their public specifications (RFC 1321 and the
//! documented `31*h + c` polynomial) so no external dependency is needed.

mod md5;

pub use md5::md5;

/// Java's `String::hashCode`: `h = 31*h + c` over UTF-16 code units, with
/// 32-bit wrapping arithmetic. Matches the JVM bit-for-bit for any string.
#[must_use]
pub fn java_string_hash(s: &str) -> i32 {
    let mut h: i32 = 0;
    for unit in s.encode_utf16() {
        // Java `char` is an unsigned 16-bit code unit, added as a non-negative int.
        h = h.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_string_hash_known_values() {
        // Verified against the JVM: "".hashCode()==0, "a"==97, "test"==3556498.
        assert_eq!(java_string_hash(""), 0);
        assert_eq!(java_string_hash("a"), 97);
        assert_eq!(java_string_hash("test"), 3_556_498);
    }
}
