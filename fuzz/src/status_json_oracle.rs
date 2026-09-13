//! Independent field model for a server-list status response.
//!
//! This deliberately uses only `serde_json::Value` and small caller-owned
//! structs. It does not call the production status parser or the production
//! text-component tree while deriving expected fields.

use lodestone_net::{PlayerSample, ServerStatus};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedStatus {
    pub motd: Option<String>,
    pub online: Option<u32>,
    pub max: Option<u32>,
    pub sample: Vec<ExpectedSample>,
    pub version: Option<String>,
    pub protocol: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedSample {
    pub name: String,
    pub id: Option<String>,
}

/// Extracts the fields that the production status parser exposes.
///
/// `Err(())` means the document is not valid JSON or is not a JSON object;
/// that is the production parser's only document-level error condition. A
/// `None` MOTD means the description used a component shape outside this
/// deliberately small independent model, so callers should compare the
/// scalar status fields but leave the text fold to its own target.
pub fn expected_status(json: &str) -> Result<ExpectedStatus, ()> {
    let root: Value = serde_json::from_str(json).map_err(|_| ())?;
    let object = root.as_object().ok_or(())?;

    let players = object.get("players").and_then(Value::as_object);
    let count = |key: &str| {
        players
            .and_then(|value| value.get(key))
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
    };
    let sample = players
        .and_then(|value| value.get("sample"))
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(expected_sample).collect())
        .unwrap_or_default();

    let version = object.get("version").and_then(Value::as_object);
    let version_name = version
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let protocol = version
        .and_then(|value| value.get("protocol"))
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());

    let motd = object
        .get("description")
        .map(simple_motd)
        .unwrap_or(Some(String::new()));

    Ok(ExpectedStatus {
        motd,
        online: count("online"),
        max: count("max"),
        sample,
        version: version_name,
        protocol,
    })
}

fn expected_sample(value: &Value) -> Option<ExpectedSample> {
    let object = value.as_object()?;
    let name = object.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    Some(ExpectedSample {
        name: name.to_owned(),
        id: object
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn simple_motd(value: &Value) -> Option<String> {
    let mut out = String::new();
    append_simple_motd(value, &mut out, 0)?;
    Some(out)
}

/// Models the literal/sequence/`text`/`extra` subset without borrowing the
/// production `Text` parser. Translation components are intentionally outside
/// this model: their wording depends on fallback and argument substitution.
fn append_simple_motd(value: &Value, out: &mut String, depth: usize) -> Option<()> {
    if depth > 64 {
        return None;
    }
    match value {
        // The production text parser has its own number spelling rules. Keep
        // this model deliberately independent by checking only string-shaped
        // component text, which covers real status responses without creating
        // a duplicate numeric parser.
        Value::Null | Value::Bool(_) | Value::Number(_) => return None,
        Value::String(value) => append_literal(value, out),
        Value::Array(values) => {
            for value in values {
                append_simple_motd(value, out, depth + 1)?;
            }
        }
        Value::Object(object) => {
            if object
                .get("translate")
                .and_then(Value::as_str)
                .is_some()
                && object.get("text").and_then(Value::as_str).is_none()
            {
                return None;
            }
            if let Some(text) = object.get("text").and_then(Value::as_str) {
                append_literal(text, out);
            }
            if let Some(Value::Array(extra)) = object.get("extra") {
                for value in extra {
                    append_simple_motd(value, out, depth + 1)?;
                }
            }
        }
    }
    Some(())
}

/// Removes the section-sign formatting pairs using the wire format's
/// independent two-character consumption rule. A dangling prefix is dropped.
fn append_literal(value: &str, out: &mut String) {
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character == '\u{00a7}' {
            let _ = chars.next();
        } else {
            out.push(character);
        }
    }
}

/// Compares the production result with the independent scalar/text model.
///
/// This is kept as a helper so the unit test can run the same assertion with a
/// deliberately modified expected value as its negative control.
pub fn assert_matches(actual: &ServerStatus, expected: &ExpectedStatus) {
    assert_eq!(actual.online, expected.online, "online player count");
    assert_eq!(actual.max, expected.max, "maximum player count");
    assert_eq!(actual.version, expected.version, "server version name");
    assert_eq!(actual.protocol, expected.protocol, "server protocol");

    let expected_sample: Vec<PlayerSample> = expected
        .sample
        .iter()
        .map(|sample| PlayerSample {
            name: sample.name.clone(),
            id: sample.id.clone(),
        })
        .collect();
    assert_eq!(actual.sample, expected_sample, "player sample");
    if let Some(motd) = &expected.motd {
        assert_eq!(&actual.motd, motd, "MOTD");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn captured_status_matches_the_independent_model() {
        let json = include_str!("../seeds/status_json_model/vanilla_status_response_26_2.json");
        let expected = expected_status(json).expect("captured status JSON is an object");
        let actual = lodestone_net::parse_status_json(json, None)
            .expect("captured status JSON parses");
        assert_matches(&actual, &expected);
    }

    #[test]
    #[should_panic(expected = "online player count")]
    fn wrong_external_player_count_is_detected() {
        let json = include_str!("../seeds/status_json_model/vanilla_status_response_26_2.json");
        let mut expected = expected_status(json).expect("captured status JSON is an object");
        let actual = lodestone_net::parse_status_json(json, None)
            .expect("captured status JSON parses");
        expected.online = Some(expected.online.expect("capture reports an online player") + 1);
        assert_matches(&actual, &expected);
    }

    #[test]
    fn captured_fixture_agrees_with_its_external_summary() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../crates/versions/26.2/tests/fixtures/vanilla_status_response_26_2.json"
        ))
        .expect("captured fixture is valid JSON");
        let raw = fixture["status_json_raw"]
            .as_str()
            .expect("fixture carries raw status JSON");
        let summary = &fixture["status_json_parsed"];
        let actual = lodestone_net::parse_status_json(raw, None)
            .expect("captured status JSON parses");

        assert_eq!(actual.motd, summary["description"].as_str().unwrap());
        assert_eq!(
            actual.online,
            summary["players"]["online"].as_u64().map(|v| v as u32)
        );
        assert_eq!(
            actual.max,
            summary["players"]["max"].as_u64().map(|v| v as u32)
        );
        assert_eq!(actual.version.as_deref(), summary["version"]["name"].as_str());
        assert_eq!(
            actual.protocol,
            summary["version"]["protocol"].as_i64().map(|v| v as i32)
        );
    }
}
