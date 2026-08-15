//! The `textures` profile property: base64 → JSON → a typed skin declaration,
//! including the **wide/slim model** every player rig needs and nothing in this
//! workspace decoded before.
//!
//! # Where the record definition comes from
//!
//! Not from a community wiki, and not from the decompiled `client-src` — the
//! payload is `com.mojang.authlib`'s, and authlib is a *library*, shipped as a
//! jar rather than as source. The shape below was read out of the real jar
//! 26.2 itself resolves,
//! `.cache/mc/26.2/libraries/com/mojang/authlib/9.0.75/authlib-9.0.75.jar`, by
//! walking the constant pool of three classes:
//!
//! | class | what it fixes |
//! |---|---|
//! | `yggdrasil/response/MinecraftTexturesPayload` | a record of `timestamp: long`, `profileId: UUID`, `profileName: String`, `isPublic: boolean`, `textures: Map` |
//! | `minecraft/MinecraftProfileTexture$Type` | the map's keys, an enum of exactly `SKIN`, `CAPE`, `ELYTRA` — GSON writes an enum key as its own name, so those are the literal JSON keys |
//! | `minecraft/MinecraftProfileTexture` | each value's fields: `url: String` and `metadata: Map<String, String>`, read through `getMetadata(key)` |
//!
//! and the *consumer* is 26.2's own source, which is available:
//! `SkinManager.registerTextures` does exactly
//!
//! ```text
//! model = PlayerModelType.byLegacyServicesName(skinInfo.getMetadata("model"));
//! ```
//!
//! so the model lives on the **skin** texture's metadata under the key
//! `"model"`, and nowhere else.
//!
//! # The trap in `PlayerModelType`, and it is a real one
//!
//! `PlayerModelType`
//! (`.cache/mc/26.2/client-src/net/minecraft/world/entity/player/PlayerModelType.java`)
//! carries **two** names per variant, and they are not the same string:
//!
//! ```text
//! SLIM("slim", "slim"),
//! WIDE("wide", "default");
//! ```
//!
//! The first is the `id` (the `StringRepresentable` serialized name, used by
//! the datapack `CODEC` and by the `skin_patch` item component); the second is
//! the `legacyServicesId`, and **that** is what the session service writes into
//! `metadata.model`. So the wide value on the wire here is `"default"`, not
//! `"wide"` — matching on `"wide"` finds nothing and, because
//! `byLegacyServicesName` is `requireNonNullElse(…, WIDE)`, still *appears* to
//! work: every skin resolves wide, including every slim one, and the only
//! symptom is Alex's arms being one pixel too thick. [`PlayerModelType::WIDE_LEGACY_SERVICES_ID`]
//! is published so a caller cannot restate the wrong one.
//!
//! Absence is also wide, deliberately and not defensively: a texture pack-less
//! account, a texture with no `metadata` map at all, and an unrecognised value
//! all resolve to [`PlayerModelType::Wide`], which is exactly
//! `Objects.requireNonNullElse(NAME_LOOKUP.apply(name), WIDE)`.
//!
//! # Why base64 is decoded here rather than by a crate
//!
//! `base64` is a workspace dependency already (`lodestone-auth` uses it for
//! PKCE), but adding it to *this* crate's manifest rewrites `Cargo.lock`, which
//! is off-limits to this cluster. The decoder below is the standard RFC 4648
//! alphabet in about thirty lines and is gated against RFC 4648 §10's own test
//! vectors — an outside expectation in the strict sense, since the vectors
//! predate this file by twenty years.
//!
//! # What this does *not* do
//!
//! **Fetch anything.** The URL comes back as a string; nothing here opens a
//! socket, and the texture bytes are somebody else's problem
//! ([`crate::Image::decode_png`] takes them once they arrive). Nor is the
//! Yggdrasil signature checked — `MinecraftProfileTextures.signatureState()`
//! feeds vanilla's `secure()` flag, which only gates the "require secure
//! profiles" server option; this crate never sees the signature because the
//! property's `signature` field is a sibling of `value`, not part of it.

/// Why a `textures` property could not be decoded. Its own type rather than a
/// new [`crate::AssetError`] variant: that enum is matched exhaustively in
/// several other crates, and nothing here is a resource-loading failure — this
/// is a *wire payload* parse, which happens to live in the crate that owns
/// image decoding.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SkinTextureError {
    /// The value was not base64 — the offending byte and its offset.
    #[error("textures property is not base64: byte {byte:#04x} at offset {offset}")]
    NotBase64 {
        /// The byte that is not in the alphabet.
        byte: u8,
        /// Its offset in the input.
        offset: usize,
    },
    /// The decoded bytes were not the JSON payload.
    #[error("textures payload is not JSON: {0}")]
    NotJson(String),
}

/// [`decode_textures_property`]'s result.
pub type Result<T> = std::result::Result<T, SkinTextureError>;

/// Vanilla's `PlayerModelType` — which of the two player rigs a skin declares.
///
/// See the module doc for the two-names-per-variant trap. `Wide` is the
/// default for every absent or unrecognised declaration, matching
/// `byLegacyServicesName`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayerModelType {
    /// `WIDE("wide", "default")` — the "Steve" rig, 4px arms.
    #[default]
    Wide,
    /// `SLIM("slim", "slim")` — the "Alex" rig, 3px arms.
    Slim,
}

impl PlayerModelType {
    /// The `legacyServicesId` of `WIDE`, which is **`"default"`** and not
    /// `"wide"`. Published so no caller has to remember it.
    pub const WIDE_LEGACY_SERVICES_ID: &'static str = "default";

    /// The `legacyServicesId` of `SLIM`, which happens to equal its `id`.
    pub const SLIM_LEGACY_SERVICES_ID: &'static str = "slim";

    /// `PlayerModelType.byLegacyServicesName` — `"slim"` is slim, and
    /// **everything else, including `None`, is wide**
    /// (`Objects.requireNonNullElse(NAME_LOOKUP.apply(name), WIDE)`).
    #[must_use]
    pub fn by_legacy_services_name(name: Option<&str>) -> Self {
        match name {
            Some(Self::SLIM_LEGACY_SERVICES_ID) => PlayerModelType::Slim,
            _ => PlayerModelType::Wide,
        }
    }

    /// `true` for the slim rig — the shape `lodestone_render`'s
    /// `player_model_name(slim: bool)` and `gpu/sources.rs`'s
    /// `ThirdPersonBodyState::slim` already take, so a caller does not have to
    /// re-derive the polarity. (Vanilla agrees: `PlayerModelType::STREAM_CODEC`
    /// is `BOOL.map(slim -> slim ? SLIM : WIDE)`, so `true` is slim there too.)
    #[must_use]
    pub const fn is_slim(self) -> bool {
        matches!(self, PlayerModelType::Slim)
    }

    /// The `legacyServicesId` — the spelling that appears in a `textures`
    /// property, and the exact inverse of
    /// [`by_legacy_services_name`](Self::by_legacy_services_name).
    ///
    /// Exists so a *producer* of that spelling (the skin cache's fetch
    /// writes, `<data_dir>/skin.model`) cannot pick the wrong one of the two
    /// names this type carries. Reaching for
    /// [`serialized_name`](Self::serialized_name) there writes `"wide"`, which
    /// `by_legacy_services_name` does not recognise — and because its fallback
    /// *is* wide, a Steve round-trips correctly and only an Alex is wrong.
    #[must_use]
    pub const fn legacy_services_id(self) -> &'static str {
        match self {
            PlayerModelType::Wide => Self::WIDE_LEGACY_SERVICES_ID,
            PlayerModelType::Slim => Self::SLIM_LEGACY_SERVICES_ID,
        }
    }

    /// The `id` (`StringRepresentable` serialized name) — `"wide"`/`"slim"`.
    /// This is the datapack/component spelling, **not** the one that appears in
    /// a `textures` property.
    #[must_use]
    pub const fn serialized_name(self) -> &'static str {
        match self {
            PlayerModelType::Wide => "wide",
            PlayerModelType::Slim => "slim",
        }
    }
}

/// One of vanilla's 18 identity skins — `DefaultPlayerSkin.get(uuid)`'s own
/// answer for an account that has never set a skin, or whose skin has not
/// (yet) been fetched.
///
/// # Where the record definition comes from
///
/// `DefaultPlayerSkin` (`.cache/mc/26.2/client-src/net/minecraft/client/resources/DefaultPlayerSkin.java`,
/// a client-only class) carries a fixed 18-entry table — nine slim identities
/// then the same nine wide, each `alex`/`ari`/`efe`/`kai`/`makena`/`noor`/
/// `steve`/`sunny`/`zuri` — and:
///
/// ```java
/// public static PlayerSkin get(final UUID profileId) {
///    return DEFAULT_SKINS[Math.floorMod(profileId.hashCode(), DEFAULT_SKINS.length)];
/// }
/// ```
///
/// `UUID.hashCode()` and `Long.hashCode(long)` are JDK library methods, not
/// Mojang's, so they are not in the decompile; [`default_skin_for_uuid`]'s own
/// doc cites the exact `openjdk/jdk` source read to port them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultSkin {
    /// The rig this identity's sheet is authored for.
    pub model: PlayerModelType,
    /// The jar sheet **reference** — no `assets/`/`.png` wrapping, e.g.
    /// `entity/player/wide/steve` — matching the shape
    /// [`crate::entity::EntityTexture`]'s `default_path()` and
    /// `skull_texture_stem` already return, so a caller can feed this straight
    /// into the same sheet lookup either of those does.
    pub texture: &'static str,
}

/// `DefaultPlayerSkin.DEFAULT_SKINS`, in its own declared order: nine slim
/// identities, then the same nine names wide. **Order is load-bearing** — the
/// index vanilla's uuid-hash pick lands on is this array's index, not an
/// alphabetised or regrouped one.
const DEFAULT_SKINS: [DefaultSkin; 18] = [
    DefaultSkin { model: PlayerModelType::Slim, texture: "entity/player/slim/alex" },
    DefaultSkin { model: PlayerModelType::Slim, texture: "entity/player/slim/ari" },
    DefaultSkin { model: PlayerModelType::Slim, texture: "entity/player/slim/efe" },
    DefaultSkin { model: PlayerModelType::Slim, texture: "entity/player/slim/kai" },
    DefaultSkin { model: PlayerModelType::Slim, texture: "entity/player/slim/makena" },
    DefaultSkin { model: PlayerModelType::Slim, texture: "entity/player/slim/noor" },
    DefaultSkin { model: PlayerModelType::Slim, texture: "entity/player/slim/steve" },
    DefaultSkin { model: PlayerModelType::Slim, texture: "entity/player/slim/sunny" },
    DefaultSkin { model: PlayerModelType::Slim, texture: "entity/player/slim/zuri" },
    DefaultSkin { model: PlayerModelType::Wide, texture: "entity/player/wide/alex" },
    DefaultSkin { model: PlayerModelType::Wide, texture: "entity/player/wide/ari" },
    DefaultSkin { model: PlayerModelType::Wide, texture: "entity/player/wide/efe" },
    DefaultSkin { model: PlayerModelType::Wide, texture: "entity/player/wide/kai" },
    DefaultSkin { model: PlayerModelType::Wide, texture: "entity/player/wide/makena" },
    DefaultSkin { model: PlayerModelType::Wide, texture: "entity/player/wide/noor" },
    DefaultSkin { model: PlayerModelType::Wide, texture: "entity/player/wide/steve" },
    DefaultSkin { model: PlayerModelType::Wide, texture: "entity/player/wide/sunny" },
    DefaultSkin { model: PlayerModelType::Wide, texture: "entity/player/wide/zuri" },
];

/// `DefaultPlayerSkin.getDefaultSkin()` — index `6`, `wide/steve` — vanilla's
/// answer when no uuid is available at all. Distinct from
/// [`default_skin_for_uuid`], which is the uuid-hash pick every *identified*
/// account (including every offline-mode one, whose uuid is derived from its
/// username rather than absent) actually gets.
#[must_use]
pub const fn default_skin() -> DefaultSkin {
    DEFAULT_SKINS[6]
}

/// `DefaultPlayerSkin.get(UUID)` — the uuid-hash pick over the 18 built-in
/// identities, for a profile that has declared no skin at all (or one not yet
/// fetched). This is **one resolver, shared by every consumer that only has a
/// uuid to go on**: a player-head item's owner (once threaded through), a
/// player entity with no `textures` property, and the local player's own
/// avatar before sign-in or before a fetch lands — so the same uuid always
/// picks the same identity everywhere it is asked, rather than each call site
/// defaulting independently and disagreeing (the wide-in-world/slim-in-inventory
/// split this function exists to close).
///
/// # Signature: the Java-shaped halves, not this crate's own uuid type
///
/// This crate takes no dependency on the `uuid` crate (every consumer of this
/// function already has one, gated behind their own feature or none at all), so
/// the two 64-bit halves are the parameter — exactly the shape
/// `java.util.UUID`'s own two private fields take, and exactly what
/// `uuid::Uuid::as_u64_pair()` returns, reinterpreted as signed: `let (hi, lo) =
/// uuid.as_u64_pair(); default_skin_for_uuid(hi as i64, lo as i64)`.
///
/// # Ported from the method bodies, not restated from memory
///
/// Three JDK/Mojang methods chain into this one value, each read from its own
/// source rather than transcribed from familiarity — the discipline this
/// codebase asks of every port:
///
/// ```java
/// // DefaultPlayerSkin.java (26.2 client decompile)
/// DEFAULT_SKINS[Math.floorMod(profileId.hashCode(), DEFAULT_SKINS.length)]
///
/// // java.util.UUID (openjdk/jdk, java.base/java.util.UUID -- not Mojang's,
/// // so not in the 26.2 decompile; read from the real JDK source)
/// public int hashCode() {
///     return Long.hashCode(mostSigBits ^ leastSigBits);
/// }
///
/// // java.lang.Long (openjdk/jdk, java.base/java.lang.Long)
/// public static int hashCode(long value) {
///     return (int)(value ^ (value >>> 32));
/// }
/// ```
///
/// `>>> `is Java's **unsigned** right shift. The final truncating `(int)` cast
/// keeps only the low 32 bits either way, and XORing two 32-bit halves whose
/// upper half only differs by its fill bits (sign- vs zero-extended) leaves
/// that upper half discarded regardless — so an arithmetic and a logical shift
/// by exactly 32 give the identical final `i32` here. Implemented with an
/// explicit `u64` shift anyway, matching the Java operator literally rather
/// than leaning on that equivalence.
///
/// `Math.floorMod` for a positive divisor is "the least non-negative
/// remainder", which is exactly [`i32::rem_euclid`]'s contract for a positive
/// argument.
#[must_use]
pub fn default_skin_for_uuid(most_sig_bits: i64, least_sig_bits: i64) -> DefaultSkin {
    let hilo = most_sig_bits ^ least_sig_bits;
    // `Long.hashCode(long)`: `(int)(value ^ (value >>> 32))`, `>>>` unsigned.
    let long_hash = hilo ^ (((hilo as u64) >> 32) as i64);
    let hash = long_hash as i32;
    let index = hash.rem_euclid(DEFAULT_SKINS.len() as i32) as usize;
    DEFAULT_SKINS[index]
}

/// One entry of `MinecraftTexturesPayload.textures` — a URL plus its declared
/// model. `model` is meaningful only for the `SKIN` entry; a cape or elytra
/// carries no `metadata.model` and so reads as [`PlayerModelType::Wide`],
/// which is why [`ProfileTextures`] exposes the cape and elytra as bare URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkinTexture {
    /// The texture URL, verbatim. Not fetched, not validated as a URL, and
    /// **not** host-checked — vanilla's own `TextureUrlChecker` restricts the
    /// allowed hosts, and a caller that goes on to fetch this must do the same.
    pub url: String,
    /// The declared rig, from `metadata.model` via
    /// [`PlayerModelType::by_legacy_services_name`].
    pub model: PlayerModelType,
}

/// A decoded `textures` profile property.
///
/// Every field is optional because the payload's `textures` map is: an account
/// that has never set a skin sends `{}`, and vanilla falls back to
/// `DefaultPlayerSkin.get(profileId)` (a UUID hash over the eight built-in
/// sheets) in that case — a fallback this type deliberately does not perform,
/// so a caller can tell "no skin declared" from "a skin declared as wide".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfileTextures {
    /// The `SKIN` entry: the 64×64 body sheet and the declared rig.
    pub skin: Option<SkinTexture>,
    /// The `CAPE` entry's URL.
    pub cape: Option<String>,
    /// The `ELYTRA` entry's URL. Present since 1.19 and independent of the
    /// cape, though in practice Mojang serves the same image.
    pub elytra: Option<String>,
    /// `profileName`, when the payload carries one. Useful only as a
    /// cross-check that the blob belongs to the profile it was attached to.
    pub profile_name: Option<String>,
}

impl ProfileTextures {
    /// The declared rig, or [`PlayerModelType::Wide`] when no skin is declared
    /// at all — the same collapse `SkinManager.registerTextures` performs when
    /// `textures.skin()` is null and it falls through to
    /// `DefaultPlayerSkin.get(profileId).model()` (whose own default is wide
    /// for all but three of the eight built-ins; this port does not model the
    /// UUID-hash pick, so it reports wide).
    #[must_use]
    pub fn model(&self) -> PlayerModelType {
        self.skin.as_ref().map_or(PlayerModelType::Wide, |s| s.model)
    }
}

/// Decodes a `textures` profile-property **value** (the base64 blob straight
/// off `player_info`'s `ADD_PLAYER`, or off a login profile property) into
/// [`ProfileTextures`].
///
/// Errors on a value that is not base64 or not JSON. A *well-formed* payload
/// with no textures at all is [`ProfileTextures::default`] and not an error —
/// that is the ordinary skinless account.
pub fn decode_textures_property(value: &str) -> Result<ProfileTextures> {
    let bytes = base64_decode(value)?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| SkinTextureError::NotJson(e.to_string()))?;
    Ok(textures_from_json(&json))
}

/// The JSON half of [`decode_textures_property`], split out so a gate can feed
/// a hand-written payload without base64-encoding it first.
#[must_use]
pub fn textures_from_json(json: &serde_json::Value) -> ProfileTextures {
    let textures = json.get("textures");
    let entry = |key: &str| textures.and_then(|t| t.get(key));
    let url_of = |v: Option<&serde_json::Value>| {
        v.and_then(|e| e.get("url"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };

    let skin_entry = entry("SKIN");
    let skin = url_of(skin_entry).map(|url| {
        // `getMetadata("model")` — a lookup into a `Map<String, String>`, so a
        // missing `metadata` object and a missing `model` key are the same
        // thing, and both mean wide.
        let declared = skin_entry
            .and_then(|e| e.get("metadata"))
            .and_then(|m| m.get("model"))
            .and_then(serde_json::Value::as_str);
        SkinTexture {
            url,
            model: PlayerModelType::by_legacy_services_name(declared),
        }
    });

    ProfileTextures {
        skin,
        cape: url_of(entry("CAPE")),
        elytra: url_of(entry("ELYTRA")),
        profile_name: json
            .get("profileName")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    }
}

/// RFC 4648 base64 decode, standard alphabet, padding optional. Accepts the
/// URL-safe `-`/`_` substitutions too (harmless, and a caller pasting a blob
/// out of a URL query is a real accident); rejects any other byte, so a
/// truncated or wrapped blob fails loudly rather than decoding to garbage that
/// then fails as JSON with a misleading message.
///
/// See the module doc for why this is not the `base64` crate.
fn base64_decode(input: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for (i, b) in input.bytes().enumerate() {
        let six = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b'\n' | b'\r' => continue,
            other => {
                return Err(SkinTextureError::NotBase64 {
                    byte: other,
                    offset: i,
                });
            }
        };
        acc = (acc << 6) | u32::from(six);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4648 §10's own test vectors — an expected value that originates two
    /// decades outside this file, which is the point (`decode(encode(x)) == x`
    /// against a hand-rolled encoder would prove nothing).
    #[test]
    fn base64_matches_rfc4648_section_10_vectors() {
        for (encoded, plain) in [
            ("", ""),
            ("Zg==", "f"),
            ("Zm8=", "fo"),
            ("Zm9v", "foo"),
            ("Zm9vYg==", "foob"),
            ("Zm9vYmE=", "fooba"),
            ("Zm9vYmFy", "foobar"),
        ] {
            let got = base64_decode(encoded).expect(encoded);
            assert_eq!(
                got,
                plain.as_bytes(),
                "{encoded:?} should decode to {plain:?}, got {:?}",
                String::from_utf8_lossy(&got)
            );
        }
    }

    /// Unpadded input decodes identically — Mojang pads, but a blob that has
    /// been through a URL or a config file may not.
    #[test]
    fn padding_is_optional() {
        assert_eq!(base64_decode("Zm9vYmE").unwrap(), b"fooba");
        assert_eq!(base64_decode("Zg").unwrap(), b"f");
    }

    /// A byte outside the alphabet is an error, not silently skipped. The
    /// control: the same string with the offending byte removed decodes.
    #[test]
    fn a_non_base64_byte_is_an_error() {
        assert!(base64_decode("Zm9v!YmFy").is_err());
        assert!(base64_decode("Zm9vYmFy").is_ok());
    }

    /// The payload shape, hand-built from the authlib record definition in the
    /// module doc (record fields `profileName`/`textures`; map keys
    /// `SKIN`/`CAPE`/`ELYTRA`; each value `url` + `metadata`).
    fn payload(model: Option<&str>) -> serde_json::Value {
        let metadata = match model {
            Some(m) => serde_json::json!({ "model": m }),
            None => serde_json::Value::Null,
        };
        let mut skin = serde_json::json!({ "url": "https://textures.example.invalid/skin" });
        if !metadata.is_null() {
            skin["metadata"] = metadata;
        }
        serde_json::json!({
            "timestamp": 1_700_000_000_000i64,
            "profileId": "069a79f444e94726a5befca90e38aaf5",
            "profileName": "Notch",
            "textures": {
                "SKIN": skin,
                "CAPE": { "url": "https://textures.example.invalid/cape" },
            }
        })
    }

    /// **The load-bearing test.** `"slim"` is slim; `"default"` is wide; and
    /// *absent* is wide. The wide case is asserted through the real
    /// `legacyServicesId` (`"default"`) *and* through the wrong-hypothesis
    /// spelling (`"wide"`), which must also resolve wide — because if a future
    /// edit matched on `"wide"` instead, both of those would still pass while
    /// `"default"` silently broke. So `"default"` is asserted to be wide and
    /// `"slim"` to be slim, and the pair cannot both hold under the swapped
    /// implementation.
    #[test]
    fn the_model_comes_from_the_legacy_services_name_not_the_id() {
        assert_eq!(
            textures_from_json(&payload(Some("slim"))).model(),
            PlayerModelType::Slim
        );
        assert_eq!(
            textures_from_json(&payload(Some(PlayerModelType::WIDE_LEGACY_SERVICES_ID))).model(),
            PlayerModelType::Wide
        );
        assert_eq!(PlayerModelType::WIDE_LEGACY_SERVICES_ID, "default");
        // An unrecognised value (including the *id* spelling "wide", which
        // never appears in a real payload) is wide, per
        // `requireNonNullElse(..., WIDE)`.
        assert_eq!(
            textures_from_json(&payload(Some("wide"))).model(),
            PlayerModelType::Wide
        );
        assert_eq!(
            textures_from_json(&payload(None)).model(),
            PlayerModelType::Wide
        );
        assert!(PlayerModelType::Slim.is_slim());
        assert!(!PlayerModelType::Wide.is_slim());
    }

    #[test]
    fn urls_and_profile_name_survive_the_walk() {
        let t = textures_from_json(&payload(Some("slim")));
        assert_eq!(
            t.skin.as_ref().map(|s| s.url.as_str()),
            Some("https://textures.example.invalid/skin")
        );
        assert_eq!(t.cape.as_deref(), Some("https://textures.example.invalid/cape"));
        assert_eq!(t.elytra, None);
        assert_eq!(t.profile_name.as_deref(), Some("Notch"));
    }

    /// A skinless account: a well-formed payload with an empty `textures` map
    /// is not an error, and reports "no skin" rather than "a wide skin" —
    /// distinguishable, which is why `skin` is an `Option` and `model()` is a
    /// separate call.
    #[test]
    fn an_empty_textures_map_is_not_an_error_and_is_distinguishable() {
        let json = serde_json::json!({ "profileName": "Nobody", "textures": {} });
        let t = textures_from_json(&json);
        assert_eq!(t.skin, None);
        assert_eq!(t.model(), PlayerModelType::Wide);
        assert_eq!(t, ProfileTextures { profile_name: Some("Nobody".into()), ..Default::default() });
    }

    /// End to end through base64, using this test's *own* encoder so the
    /// decoder is not being checked against itself: the encoder is three lines
    /// and its correctness is already pinned by the RFC vectors above running
    /// through the decoder.
    #[test]
    fn a_base64_wrapped_payload_round_trips_through_the_public_entry_point() {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let plain = payload(Some("slim")).to_string().into_bytes();
        let mut encoded = String::new();
        for chunk in plain.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
            for i in 0..4 {
                if i <= chunk.len() {
                    encoded.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3F) as usize] as char);
                } else {
                    encoded.push('=');
                }
            }
        }
        let t = decode_textures_property(&encoded).expect("our own encoding must decode");
        assert_eq!(t.model(), PlayerModelType::Slim);
        assert_eq!(t.profile_name.as_deref(), Some("Notch"));

        // The control: garbage in is an error, so the success above is not
        // vacuous.
        assert!(decode_textures_property("not base64 at all!!").is_err());
    }
}
