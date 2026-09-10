//! JSON parsing for pack-authored documents, with vanilla's own tolerance.
//!
//! ## What it is
//!
//! One function, [`from_slice_lenient`], for reading a JSON document that came
//! out of a **resource pack** rather than out of our own encoder. It parses the
//! first JSON value in the input and ignores whatever follows it, which is what
//! vanilla does and what `serde_json::from_slice` does not.
//!
//! ## Why it exists
//!
//! `serde_json::from_slice` requires the input to be *exactly* one value:
//! anything after the closing brace is `trailing characters at line N column M`.
//! Vanilla's own JSON reader for pack-authored documents runs in a strict
//! mode (so the *value itself* is still strict JSON — no comments, no
//! unquoted keys, no single quotes) but then reads exactly one value from it
//! **without a following end-of-document assertion**. That single-value read
//! consumes one value and stops, so a document with a stray extra `}` after
//! the root object parses fine in the real client. Note this is specifically
//! the *hand-rolled*, tolerant reading path vanilla uses for pack content: a
//! stricter one-shot parse-and-deserialize call would have asserted full
//! consumption, and vanilla deliberately routes pack-authored JSON through the
//! tolerant path instead of that stricter one. See `docs/resource-packs.md`
//! for more detail.
//!
//! That difference is not academic. Measured on a real, widely-used third-party
//! 16× pack: 23 of its `textures/item/*.png.mcmeta` files end with one extra
//! closing brace. Vanilla renders those 23 items normally; we rejected the
//! metadata, and `AtlasBuilder::load` treats a metadata failure as a failure of
//! the *texture*, so all 23 items dropped out of the stitched item atlas and
//! drew as empty wells. Nothing was red, and the only visible symptom was
//! "some of this pack's item art does not apply".
//!
//! ## How to change it
//!
//! Use this for anything a pack author wrote; keep `serde_json::from_slice` for
//! anything we produced ourselves, where trailing bytes really are a bug worth
//! failing on. The leniency is *only* about what comes after the value — a
//! malformed value is still an error, which is what keeps this from silently
//! accepting half a document.

use serde::de::DeserializeOwned;

/// Parses the first JSON value in `bytes` and ignores anything after it —
/// vanilla's own tolerant-parsing behaviour for pack-authored documents.
///
/// Strict about the value itself: a truncated or malformed document is still an
/// error. Only *trailing* content is tolerated.
///
/// The result type is generic so a parser can deserialize a named transport
/// DTO directly while retaining the same trailing-content tolerance.
///
/// # Errors
///
/// Returns the underlying [`serde_json::Error`] when the input holds no JSON
/// value at all, or when the first value is malformed.
pub(crate) fn from_slice_lenient<T>(bytes: &[u8]) -> Result<T, serde_json::Error>
where
    T: DeserializeOwned,
{
    let mut stream = serde_json::Deserializer::from_slice(bytes).into_iter::<T>();
    match stream.next() {
        Some(result) => result,
        // An empty (or whitespace-only) input yields no item at all. Ask
        // `serde_json` for its own EOF error rather than inventing one, so the
        // message a caller surfaces is the same as the strict path's.
        None => match serde_json::from_slice::<T>(b"") {
            Ok(_) => unreachable!("empty input is never valid JSON"),
            Err(error) => Err(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::from_slice_lenient;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct TestDocument {
        animation: TestAnimation,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct TestAnimation {
        frametime: u32,
    }

    /// The exact shape measured in a real pack: one closing brace too many.
    /// This is the whole reason the module exists, so it is the first gate.
    #[test]
    fn a_stray_trailing_brace_is_ignored_the_way_gson_ignores_it() {
        let doc = br#"{
	"animation": {
		"interpolate": false,
		"frametime": 2
		}
	}
}"#;
        // The control: the strict parser this replaced rejects it, so the pass
        // below is an observation about the change rather than about the input.
        assert!(
            serde_json::from_slice::<TestDocument>(doc).is_err(),
            "control failed: serde_json already accepts trailing content, so this \
             module would be a no-op"
        );
        let value: TestDocument = from_slice_lenient(doc).expect("the first value must parse");
        assert_eq!(
            value.animation.frametime, 2,
            "the value itself must still be read in full, not merely accepted"
        );
    }

    /// Leniency is about what comes *after* the value. A value that is itself
    /// broken must still fail, or this would quietly accept half a document and
    /// hand back a partial object.
    #[test]
    fn a_malformed_value_is_still_an_error() {
        assert!(from_slice_lenient::<TestDocument>(br#"{"animation": {"#).is_err());
        assert!(from_slice_lenient::<TestDocument>(b"").is_err());
        assert!(from_slice_lenient::<TestDocument>(b"   \n\t ").is_err());
        // Vanilla's strict value-parsing mode still rejects JSON5-isms, and so
        // must this: the divergence being fixed is trailing content only.
        assert!(from_slice_lenient::<TestDocument>(br#"{animation: 1}"#).is_err());
    }

    /// An ordinary, well-formed document must be unaffected — the overwhelmingly
    /// common case, and the one a regression here would break silently.
    #[test]
    fn a_well_formed_document_parses_identically() {
        let doc = br#"{"animation":{"frametime":3},"texture":{"blur":true}}"#;
        let lenient: TestDocument = from_slice_lenient(doc).expect("lenient");
        let strict: TestDocument = serde_json::from_slice(doc).expect("strict");
        assert_eq!(lenient, strict);
    }
}
