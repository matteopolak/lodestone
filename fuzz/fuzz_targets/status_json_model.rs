//! libFuzzer target for server-list status JSON.
//!
//! A server controls this document before a client joins, and the status
//! parser feeds its description, player sample, version and favicon into the
//! server-list UI. The target compares the production parser's scalar fields
//! against an independent `serde_json::Value` model rather than merely
//! asserting that malformed input does not panic. The small literal/sequence
//! MOTD subset is also checked without calling the production text parser.
//!
//! Inputs that are not UTF-8 are outside the JSON API and are ignored. Invalid
//! JSON and non-object JSON must produce the production parser's typed error;
//! a successful parse must agree with the independent model. The seed is the
//! raw status response captured from a real 26.2 server, not output from a
//! Lodestone encoder.

#![no_main]

use std::str;

use libfuzzer_sys::fuzz_target;
use lodestone_fuzz_targets::status_json_oracle::{assert_matches, expected_status};
use lodestone_net::parse_status_json;

fuzz_target!(|data: &[u8]| {
    let Ok(json) = str::from_utf8(data) else {
        return;
    };

    let expected = match expected_status(json) {
        Ok(expected) => expected,
        Err(()) => {
            assert!(
                parse_status_json(json, None).is_err(),
                "the production parser accepted invalid or non-object JSON"
            );
            return;
        }
    };

    let actual = parse_status_json(json, None)
        .expect("a valid JSON object must produce a status result");
    assert_matches(&actual, &expected);
});
