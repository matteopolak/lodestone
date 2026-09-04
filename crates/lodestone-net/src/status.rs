//! Decoding a status-ping response into the fields a server list actually
//! renders: MOTD text, player counts, version name, and the favicon image.
//!
//! [`crate::ping`] deliberately hands back the status JSON unparsed, because the
//! schema drifts across versions and forks. That is the right call for the
//! transport layer, but every caller then needs the same handful of fields, and
//! "each caller decodes it itself" is how three subtly different MOTD flatteners
//! end up in the tree. This module is that one decoder.
//!
//! ## What it tolerates
//!
//! Real servers in the wild are not consistent, so every field is optional and
//! several have more than one accepted shape:
//!
//! * `description` is either a **plain string** (very common on proxies and
//!   older servers) or a **chat component** object with `text`/`extra`/`translate`
//!   — the latter nests arbitrarily deep, so flattening recurses.
//! * `players.online` / `players.max` are missing entirely on some proxies that
//!   hide their player counts.
//! * `favicon` is a `data:image/png;base64,…` URI. A few servers omit the MIME
//!   prefix, emit `\n` inside the payload, or use the URL-safe alphabet.
//!
//! Anything unparseable degrades to `None` rather than failing the whole status:
//! a server with a broken favicon should still show its MOTD.

use crate::error::{NetError, Result};

/// The maximum decoded favicon size accepted, in bytes.
///
/// Vanilla favicons are 64×64 PNGs (a few KiB). This is a generous ceiling that
/// still stops a hostile server from making a server-list entry allocate
/// unboundedly — the status JSON itself is already length-capped by
/// [`crate::ping`], but base64 expands ~4:3 and this keeps the intent explicit.
pub const MAX_FAVICON_BYTES: usize = 1 << 20;

/// One entry from `players.sample[]` — a player the server reports as online.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerSample {
    /// The player's display name.
    pub name: String,
    /// The player's profile id (a UUID), when the server sent one.
    pub id: Option<String>,
}

/// A status response decoded into the fields a server list renders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerStatus {
    /// The MOTD, flattened to plain text with formatting codes stripped.
    /// Multi-line MOTDs keep their `\n`.
    ///
    /// This is the *wording*, for layout and for anything that needs a string.
    /// It is derived from [`Self::motd_spans`] rather than separately parsed, so
    /// the two cannot disagree about what the server said.
    pub motd: String,
    /// The MOTD as styled runs, carrying whatever colour and formatting the
    /// server sent — both modern component `color` keys and legacy `§` codes.
    ///
    /// A server list that draws [`Self::motd`] shows the right words in one flat
    /// colour, which is what this client did: the old decoder read only
    /// `text`/`translate`/`extra` and then ran a `strip_formatting` pass that
    /// deleted every `§` pair, so colour was discarded twice before any renderer
    /// could have used it. Draw this field instead.
    pub motd_spans: Vec<lodestone_model::text::TextSpan>,
    /// Players currently online, when the server reports it.
    pub online: Option<u32>,
    /// Player slots, when the server reports it.
    pub max: Option<u32>,
    /// Online players' names, from `players.sample[]`, in server order.
    ///
    /// Vanilla shows these in the server list row's "who's online" tooltip.
    /// A server that omits the sample —
    /// which is legal and common — leaves this empty rather than failing the
    /// status, the same tolerance as every other field here.
    pub sample: Vec<PlayerSample>,
    /// Human-readable version name (e.g. `"26.2"`, `"Paper 1.21.4"`).
    pub version: Option<String>,
    /// Protocol number the server speaks, when reported.
    pub protocol: Option<i32>,
    /// Decoded favicon image bytes (PNG), when the server sent a usable one.
    pub favicon_png: Option<Vec<u8>>,
    /// Ping round-trip in milliseconds, carried through from the exchange.
    pub latency_ms: Option<u64>,
}

impl ServerStatus {
    /// Renders the player count as `online/max`, or `?` where unreported.
    #[must_use]
    pub fn players_line(&self) -> String {
        let f = |v: Option<u32>| v.map_or_else(|| "?".to_string(), |n| n.to_string());
        format!("{}/{}", f(self.online), f(self.max))
    }

    /// The MOTD's first line, which is all a one-line list row can show.
    #[must_use]
    pub fn motd_first_line(&self) -> &str {
        self.motd.split('\n').next().unwrap_or("")
    }
}

/// Decodes a raw status JSON document into a [`ServerStatus`].
///
/// `latency_ms` is threaded through from the ping exchange because it is not in
/// the JSON.
///
/// # Errors
///
/// Returns [`NetError::Protocol`] only if the document is not valid JSON or is
/// not a JSON object. Missing or malformed *fields* never error — they decode
/// to `None`, so one bad favicon cannot hide a server's MOTD.
pub fn parse_status_json(json: &str, latency_ms: Option<u64>) -> Result<ServerStatus> {
    let root: serde_json::Value = serde_json::from_str(json)
        .map_err(|_| NetError::MalformedFrame("status response is not valid JSON"))?;
    let obj = root
        .as_object()
        .ok_or(NetError::MalformedFrame("status response is not a JSON object"))?;

    let motd_spans = obj
        .get("description")
        .map(component_spans)
        .unwrap_or_default();
    let motd: String = motd_spans.iter().map(|s| s.text.as_str()).collect();

    let players = obj.get("players").and_then(|p| p.as_object());
    let count = |key: &str| -> Option<u32> {
        players
            .and_then(|p| p.get(key))
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
    };
    let sample = players
        .and_then(|p| p.get("sample"))
        .and_then(serde_json::Value::as_array)
        .map(|arr| arr.iter().filter_map(player_sample).collect())
        .unwrap_or_default();

    let version_obj = obj.get("version").and_then(|v| v.as_object());
    let version = version_obj
        .and_then(|v| v.get("name"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let protocol = version_obj
        .and_then(|v| v.get("protocol"))
        .and_then(serde_json::Value::as_i64)
        .and_then(|v| i32::try_from(v).ok());

    let favicon_png = obj
        .get("favicon")
        .and_then(serde_json::Value::as_str)
        .and_then(decode_favicon);

    Ok(ServerStatus {
        motd,
        motd_spans,
        online: count("online"),
        max: count("max"),
        sample,
        version,
        protocol,
        favicon_png,
        latency_ms,
    })
}

/// Decodes one `players.sample[]` entry, or `None` if it is unusable.
///
/// Vanilla's player-sample codec demands both `id` and `name`; real servers
/// are looser, and one malformed entry must not blank a whole row any more
/// than a broken favicon does, so an entry missing either field is skipped,
/// not fatal. The `id` is kept as the raw string rather than parsed to a
/// UUID: the list only ever displays names, and comparing an id against the
/// all-zero anonymous-profile UUID vanilla uses as a placeholder is tooltip
/// shaping, not decode.
#[must_use]
fn player_sample(v: &serde_json::Value) -> Option<PlayerSample> {
    let obj = v.as_object()?;
    let name = obj.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    Some(PlayerSample {
        name: name.to_string(),
        id: obj
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    })
}

/// Flattens a chat component (or a bare string) to **styled runs**.
///
/// Handles the shapes a `description` actually arrives in — a JSON string, an
/// array of components, an object with `text`/`extra`/`translate`, nested
/// arbitrarily deep — by handing the whole value to
/// [`Text::from_json`](lodestone_model::Text::from_json) rather than walking it
/// here. `translate` keys are emitted verbatim — resolving against the empty
/// table (`&|_| None`) lowers every key to its own name, which is a genuinely
/// resolved tree and so is what the styled flatteners accept. A real table is
/// not available here: it needs a language pack this crate has no business
/// owning, and a raw key on screen is more honest than an empty MOTD.
///
/// `to_spans`' legacy-code expansion is the load-bearing part. A great many real
/// MOTDs are legacy `§`-coded strings, including ones wrapped in modern JSON, so
/// a parse that only understood component `color` keys would still show the
/// codes as literal glyphs. The two functions this replaced did the opposite:
/// one ignored `color` entirely, the other deleted every `§` pair. This used to
/// name `to_spans_expanding_legacy`, back when a plain `to_spans` did *not*
/// expand and this was the only surface in the tree that had noticed.
///
/// **`to_spans`, not `to_interactive_spans`, and that is the right call.** A
/// `description` is an ordinary component and could in principle carry a
/// `clickEvent`/`hoverEvent`, but the decompiled server-list row this crate's
/// `motd_spans` field feeds (`ServerSelectionList`'s online-server entry) draws
/// the MOTD with a plain component-aware text blit and nothing else — no
/// `mouseClicked` override consults it, and no tooltip call is wired to it
/// either (the row's own tooltips are the ping icon and the "who's online"
/// list, both keyed off unrelated rects). Style is the whole of what a real
/// server's MOTD gets to say to this screen.
fn component_spans(v: &serde_json::Value) -> Vec<lodestone_model::text::TextSpan> {
    // `Text::from_json` parses source text, and what we hold is already a parsed
    // `Value`, so re-serialise. Round-tripping one small JSON value per ping is
    // not a cost worth a second component parser to avoid — a second parser is
    // exactly what this module's doc warns about, and what it had.
    lodestone_model::Text::from_json(&v.to_string())
        .resolve(&|_| None)
        .to_spans()
}

/// Decodes a `favicon` field into PNG bytes.
///
/// Accepts a full `data:image/png;base64,<payload>` URI (what vanilla sends) and
/// also a bare base64 payload, which some proxies emit. Whitespace inside the
/// payload is ignored — servers do wrap it. Returns `None` for anything that
/// does not decode, or that decodes to something which is not a PNG.
#[must_use]
pub fn decode_favicon(field: &str) -> Option<Vec<u8>> {
    let payload = match field.find("base64,") {
        Some(i) => &field[i + "base64,".len()..],
        // No data-URI prefix: treat the whole field as the payload, unless it is
        // a data URI in some *other* encoding, which we cannot use.
        None if field.starts_with("data:") => return None,
        None => field,
    };
    let bytes = decode_base64(payload)?;
    // A favicon that is not a PNG is a server bug or an attack; either way the
    // renderer must not be handed it.
    if bytes.starts_with(&PNG_MAGIC) {
        Some(bytes)
    } else {
        None
    }
}

/// The 8-byte PNG signature.
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Decodes standard **or** URL-safe base64, ignoring ASCII whitespace and
/// tolerating missing padding.
///
/// Hand-rolled rather than pulling a base64 crate into a dependency set this
/// crate keeps deliberately small (and which must stay wasm-clean). Returns
/// `None` on any invalid character or a truncated final group.
#[must_use]
pub fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let mut seen = 0usize;

    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        seen += 1;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
            if out.len() > MAX_FAVICON_BYTES {
                return None;
            }
        }
    }

    // A final group of exactly one base64 char encodes no whole byte and is a
    // truncation, not a valid encoding.
    if seen % 4 == 1 {
        return None;
    }
    Some(out)
}

/// Resolves and pings `host`, returning the decoded status.
///
/// This is the whole server-list operation in one call: SRV resolution per the
/// vanilla rules, the modern status exchange, and decoding. `protocol_version`
/// is advertised in the handshake; vanilla ignores it in the status state.
///
/// # Errors
///
/// Returns a [`NetError`] on DNS, connect, I/O, or protocol failure. A server
/// that answers with a usable document but a broken favicon still succeeds.
#[cfg(not(target_arch = "wasm32"))]
pub async fn server_status(
    host: &str,
    port: Option<u16>,
    protocol_version: i32,
) -> Result<ServerStatus> {
    let raw = crate::ping::ServerListPing::new(protocol_version)
        .status(host, port)
        .await?;
    parse_status_json(&raw.json, raw.latency_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vanilla-shaped status document with a component MOTD.
    const VANILLA: &str = r#"{
        "version": {"name": "26.2", "protocol": 776},
        "players": {"max": 20, "online": 3, "sample": []},
        "description": {"text": "A Minecraft Server"},
        "favicon": "data:image/png;base64,iVBORw0KGgo=",
        "enforcesSecureChat": true
    }"#;

    #[test]
    fn parses_a_vanilla_document() {
        let s = parse_status_json(VANILLA, Some(7)).unwrap();
        assert_eq!(s.motd, "A Minecraft Server");
        assert_eq!(s.online, Some(3));
        assert_eq!(s.max, Some(20));
        assert_eq!(s.version.as_deref(), Some("26.2"));
        assert_eq!(s.protocol, Some(776));
        assert_eq!(s.latency_ms, Some(7));
        assert_eq!(s.players_line(), "3/20");
        // The fixture's sample is empty, so the decode must yield an empty one.
        assert!(s.sample.is_empty());
        // `iVBORw0KGgo=` is exactly the 8-byte PNG signature.
        assert_eq!(s.favicon_png.as_deref(), Some(&PNG_MAGIC[..]));
    }

    #[test]
    fn players_sample_decodes_names_in_server_order() {
        let json = r#"{"players":{"max":100,"online":3,"sample":[
            {"name":"Notch","id":"069a79f4-44e9-4726-a5be-fca90e38aaf5"},
            {"name":"jeb_","id":"853c80ef-3c37-49fd-aa49-938b674adae6"},
            {"name":""},
            {"name":"   "},
            {"name":42}
        ]}}"#;
        let s = parse_status_json(json, None).unwrap();
        assert_eq!(s.sample.len(), 2, "blank or non-string names must be skipped");
        assert_eq!(s.sample[0].name, "Notch");
        assert_eq!(
            s.sample[0].id.as_deref(),
            Some("069a79f4-44e9-4726-a5be-fca90e38aaf5")
        );
        assert_eq!(s.sample[1].name, "jeb_");
        assert_eq!(
            s.sample[1].id.as_deref(),
            Some("853c80ef-3c37-49fd-aa49-938b674adae6")
        );
    }

    #[test]
    fn players_sample_may_be_absent_or_plain_objects() {
        // Both shapes real servers send: the key missing entirely, and a sample
        // entry without an `id` (some proxies only list names).
        let absent = parse_status_json(r#"{"players":{"online":1,"max":1}}"#, None).unwrap();
        assert!(absent.sample.is_empty());
        let no_ids = parse_status_json(
            r#"{"players":{"sample":[{"name":"only"},{"name":"names"}]}}"#,
            None,
        )
        .unwrap();
        assert_eq!(no_ids.sample.len(), 2);
        assert_eq!(no_ids.sample[1].name, "names");
        assert_eq!(no_ids.sample[1].id, None);
    }

    #[test]
    fn description_may_be_a_bare_string() {
        // Extremely common on proxies; a decoder that only handles the object
        // form shows a blank MOTD for a large slice of real servers.
        let s = parse_status_json(r#"{"description":"Hello §cthere"}"#, None).unwrap();
        assert_eq!(s.motd, "Hello there", "formatting codes must be stripped");
    }

    #[test]
    fn description_components_flatten_recursively() {
        let json = r#"{"description":{"text":"A","extra":[
            {"text":"B","extra":[{"text":"C"}]},
            {"text":"D"}
        ]}}"#;
        let s = parse_status_json(json, None).unwrap();
        assert_eq!(s.motd, "ABCD");
    }

    #[test]
    fn missing_fields_degrade_rather_than_fail() {
        let s = parse_status_json("{}", None).unwrap();
        assert_eq!(s.motd, "");
        assert_eq!(s.online, None);
        assert_eq!(s.max, None);
        assert!(s.sample.is_empty());
        assert_eq!(s.favicon_png, None);
        assert_eq!(s.players_line(), "?/?");
    }

    #[test]
    fn a_broken_favicon_does_not_hide_the_motd() {
        // The failure mode this guards: one malformed field taking the whole
        // row off the list.
        let json = r#"{"description":"live","favicon":"data:image/png;base64,!!!!"}"#;
        let s = parse_status_json(json, None).unwrap();
        assert_eq!(s.motd, "live");
        assert_eq!(s.favicon_png, None);
    }

    #[test]
    fn non_png_favicons_are_rejected() {
        // "AAAA" decodes cleanly but is not a PNG; handing those bytes to an
        // image decoder is the bug this prevents.
        let json = r#"{"favicon":"data:image/png;base64,AAAAAAAAAAA="}"#;
        assert_eq!(parse_status_json(json, None).unwrap().favicon_png, None);
    }

    #[test]
    fn favicon_payload_may_be_wrapped_or_unprefixed() {
        let wrapped = "data:image/png;base64,iVBO\nRw0K\r\nGgo=";
        assert_eq!(decode_favicon(wrapped).as_deref(), Some(&PNG_MAGIC[..]));
        assert_eq!(decode_favicon("iVBORw0KGgo=").as_deref(), Some(&PNG_MAGIC[..]));
    }

    #[test]
    fn a_non_base64_data_uri_is_refused() {
        assert_eq!(decode_favicon("data:image/png,rawbytes"), None);
    }

    /// A genuine 8×8 RGBA PNG, encoded outside this code.
    ///
    /// Produced with Python's `zlib`/`struct` (real IHDR/IDAT/IEND chunks) and
    /// base64'd with `base64.b64encode`; `file(1)` reports
    /// "PNG image data, 8 x 8, 8-bit/color RGBA, non-interlaced". The expected
    /// length and trailer below therefore originate outside `decode_base64` —
    /// the RFC vectors prove the alphabet, this proves a real image survives.
    const REAL_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAYAAADED76LAAAAbUlEQVR4n\
        BXKMRHDMAwAQFMIgt6FQnZNpZDRayh00VAeYiImRtO+h99+jPH9vQgmSdEsxjgEgklSNOvY4RQIJknRrHOHSy\
        CYJEWzrh3eAsEkKZr13uEWCCZJ0ax7h0cgmCRFs54dPgLBJCmaxR9JZYnBDqu6ZwAAAABJRU5ErkJggg==";

    #[test]
    fn a_real_png_favicon_survives_the_data_uri_round_trip() {
        let uri = format!("data:image/png;base64,{REAL_PNG_B64}");
        let bytes = decode_favicon(&uri).expect("a real PNG must decode");
        assert_eq!(bytes.len(), 166, "byte-exact length from the encoder");
        assert!(bytes.starts_with(&PNG_MAGIC));
        assert_eq!(
            &bytes[bytes.len() - 8..],
            &[0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82],
            "must end with a complete IEND chunk + CRC"
        );
    }

    #[test]
    fn base64_round_trips_known_vectors() {
        // RFC 4648 test vectors — an expected value from outside this code.
        for (enc, dec) in [
            ("", ""),
            ("Zg==", "f"),
            ("Zm8=", "fo"),
            ("Zm9v", "foo"),
            ("Zm9vYg==", "foob"),
            ("Zm9vYmE=", "fooba"),
            ("Zm9vYmFy", "foobar"),
        ] {
            assert_eq!(
                decode_base64(enc).as_deref(),
                Some(dec.as_bytes()),
                "vector {enc}"
            );
        }
        // Unpadded and URL-safe alphabets both decode.
        assert_eq!(decode_base64("Zm9vYmE").as_deref(), Some(&b"fooba"[..]));
        assert_eq!(decode_base64("-_8=").as_deref(), Some(&[0xfb, 0xff][..]));
        // Invalid characters and truncated groups are rejected.
        assert_eq!(decode_base64("Zm9v*"), None);
        assert_eq!(decode_base64("Zm9vYmFyZ"), None);
    }

    #[test]
    fn malformed_documents_error_rather_than_silently_emptying() {
        // A parse failure must be distinguishable from "server sent {}", or a
        // list row shows a blank MOTD for a server that never answered.
        assert!(parse_status_json("not json", None).is_err());
        assert!(parse_status_json("[1,2,3]", None).is_err());
    }

    #[test]
    fn motd_first_line_truncates_multiline_motds() {
        let json = r#"{"description":{"text":"line one\nline two"}}"#;
        let s = parse_status_json(json, None).unwrap();
        assert_eq!(s.motd_first_line(), "line one");
        assert!(s.motd.contains('\n'), "full MOTD keeps both lines");
    }
}
