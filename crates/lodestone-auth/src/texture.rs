//! Fetching a skin/cape texture from a URL that **arrived over the network**,
//! under vanilla's own host restriction.
//!
//! A `textures` profile property is attacker-controlled: any server can put any
//! URL in a `PlayerInfo` entry, and our own services profile is only marginally
//! more trustworthy. Vanilla does not fetch whatever it is handed — the
//! services library
//! screens every URL through a domain-allowlist check at the
//! point the payload is decoded, and logs *"Textures payload url is
//! invalid: {}"* for a rejection. [`is_allowed_texture_domain`] is that check,
//! transcribed.
//!
//! ## Where the check's definition comes from
//!
//! Not from `client-src` — the services library that defines this check ships
//! separately from the game's own decompiled source. It was read out of the real jar 26.2
//! resolves,
//! by disassembling the services library's compiled domain-check class. Both
//! the constant pool and the bytecode of the check were read, and
//! the bytecode matters: the constant pool alone shows a string-equality
//! comparison and a set-membership test without saying what either compares.
//!
//! The disassembled check, in prose: two static sets are built up front — an
//! allowed-schemes set of `"http"`/`"https"`, and an allowed-domains set
//! containing only `"textures.minecraft.net"`. The check itself: parse and
//! normalize the URL (rejecting on a parse failure); reject if the scheme is
//! absent or not in the allowed-schemes set; reject if the host is absent;
//! run the host through a Unicode (punycode) decode; reject unless the
//! decoded host is already exactly lower-case (comparing the lower-cased form
//! against the undecoded-case form and rejecting on any difference); and
//! finally test the decoded host for exact membership in the allowed-domains
//! set.
//!
//! **The two clauses nobody would invent** are the last two, and both are
//! rejections rather than normalisations:
//!
//! * the host must **already be lower-case** — comparing the lowered host
//!   against the *unlowered* one and rejecting when they differ, so
//!   `HTTPS://TEXTURES.MINECRAFT.NET/…` is refused rather
//!   than folded;
//! * the scheme accessor is likewise case-preserving, so `HTTP` is not
//!   `http` and is refused.
//!
//! Guessing at "a suffix of minecraft.net" would have been both laxer *and*
//! wrong: the allowed-domains set is exact-match set membership on the whole host, so
//! `sub.textures.minecraft.net` is not allowed either.
//!
//! ## Where this deliberately diverges, and in which direction
//!
//! Only ever **stricter**, never laxer:
//!
//! * **No punycode decode.** The Unicode (punycode) decode step is not
//!   implemented here; a host
//!   containing an `xn--` label, or any non-ASCII byte, is rejected outright.
//!   For the one allowed domain this cannot lose a legitimate URL:
//!   `textures.minecraft.net` is pure lower-case ASCII, so a punycode decode is
//!   the identity on it. It *does* reject the pathological
//!   `xn--textures-.xn--minecraft-.xn--net-`, which vanilla's services library
//!   would decode back to
//!   the allowed spelling and which no DNS resolver would ever answer.
//! * **A response size cap** ([`MAX_TEXTURE_BYTES`]). Vanilla streams the body
//!   into an image decoder with no explicit ceiling. A 64×64 skin sheet is a
//!   couple of kilobytes, so the cap costs nothing and bounds what a redirect to
//!   an allowed-host-but-enormous object can allocate.
//!
//! The structural parse is [`reqwest::Url`]'s (the `url` crate), not a
//! hand-rolled one, because that is what gets `userinfo`, ports and IPv6
//! literals right — `https://textures.minecraft.net@evil.example.invalid/x` has
//! host `evil.example.invalid` and must be refused. The raw-string inspection
//! layered on top only ever *adds* a rejection, so a mistake in it cannot widen
//! what is accepted.

use crate::error::{AuthError, Result};

/// The single host vanilla will fetch a texture from: the services library's
/// own one-element allowed-domains set. Exact match on the **whole** host, not a
/// suffix.
pub const ALLOWED_TEXTURE_DOMAIN: &str = "textures.minecraft.net";

/// The services library's own allowed-schemes set, compared **case-sensitively**
/// — see this module's docs.
pub const ALLOWED_TEXTURE_SCHEMES: [&str; 2] = ["http", "https"];

/// The ceiling on a fetched texture body. A vanilla 64×64 skin sheet is ~2–8 KiB
/// and the largest legitimate cape is smaller still, so this is three orders of
/// magnitude of headroom. Our own hardening, not vanilla's — see the module docs.
pub const MAX_TEXTURE_BYTES: usize = 512 * 1024;

/// The scheme and the host-ish region of `url` **exactly as written**, before
/// any normalisation.
///
/// [`reqwest::Url`] lower-cases both while parsing, which is precisely the
/// question vanilla asks case-sensitively, so the parsed values cannot answer
/// it. The slicing is RFC 3986's own: after `scheme://`, the authority runs to
/// the first `/`, `?` or `#`, and `userinfo` runs to the **last** `@` inside it.
/// The returned host region may still carry a `:port` — harmless, since every
/// caller only asks whether it is plain lower-case ASCII, and a port is digits.
fn raw_scheme_and_host(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    Some((scheme, host))
}

/// Whether `url` is one vanilla would fetch a texture from: the services
/// library's own domain-allowlist check.
///
/// See the module docs for the transcription, the two non-obvious rejection
/// clauses, and the two deliberate (stricter-only) divergences.
#[must_use]
pub fn is_allowed_texture_domain(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        // vanilla's own catch-and-reject on a URI parse failure.
        return false;
    };
    // The authoritative host, after `userinfo`/port/IPv6 handling.
    if parsed.host_str() != Some(ALLOWED_TEXTURE_DOMAIN) {
        return false;
    }
    let Some((raw_scheme, raw_host)) = raw_scheme_and_host(url) else {
        return false;
    };
    // Case-**sensitive**, as vanilla's own scheme accessor and set-membership test are.
    if !ALLOWED_TEXTURE_SCHEMES.contains(&raw_scheme) {
        return false;
    }
    // Vanilla's own lower-case comparison: an unlowered host is refused, not
    // folded. Plus our stricter no-punycode/no-non-ASCII rule.
    raw_host.is_ascii()
        && !raw_host.chars().any(|c| c.is_ascii_uppercase())
        && !raw_host.contains("xn--")
}

/// `GET`s a skin/cape texture, refusing any URL
/// [`is_allowed_texture_domain`] rejects **before** opening a socket.
///
/// Returns the raw body (a PNG); decoding is the caller's, since this crate has
/// no image dependency. The body is capped at [`MAX_TEXTURE_BYTES`].
///
/// # Errors
///
/// [`AuthError::TextureDomainNotAllowed`] for a refused host — with no request
/// made — and [`AuthError::Service`] for a non-success status or an oversized
/// body.
pub async fn fetch_texture(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    if !is_allowed_texture_domain(url) {
        return Err(AuthError::TextureDomainNotAllowed {
            url: url.to_owned(),
        });
    }
    let http = client.get(url).send().await?;
    let status = http.status();
    if !status.is_success() {
        return Err(AuthError::Service {
            step: "texture",
            message: format!("texture host returned {status}"),
        });
    }
    // Check the advertised length first so an oversized body is refused without
    // buffering it, then re-check the real one — `Content-Length` is a claim.
    if let Some(len) = http.content_length()
        && len > MAX_TEXTURE_BYTES as u64
    {
        return Err(AuthError::Service {
            step: "texture",
            message: format!("texture is {len} bytes, over the {MAX_TEXTURE_BYTES} cap"),
        });
    }
    let bytes = http.bytes().await?;
    if bytes.len() > MAX_TEXTURE_BYTES {
        return Err(AuthError::Service {
            step: "texture",
            message: format!(
                "texture is {} bytes, over the {MAX_TEXTURE_BYTES} cap",
                bytes.len()
            ),
        });
    }
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real services skin URL, both schemes. The hash is not a real one, but
    /// the shape is: `/texture/<sha>`.
    #[test]
    fn the_real_texture_host_is_allowed_on_both_schemes() {
        assert!(is_allowed_texture_domain(
            "http://textures.minecraft.net/texture/1a2b3c"
        ));
        assert!(is_allowed_texture_domain(
            "https://textures.minecraft.net/texture/1a2b3c"
        ));
        // A port and a query are not by themselves disqualifying: vanilla looks
        // at the scheme and the host and nothing else.
        assert!(is_allowed_texture_domain(
            "https://textures.minecraft.net:443/texture/1a2b3c?v=2"
        ));
    }

    /// The rejections, each one a distinct way a hostile URL tries to look
    /// allowed. Every stand-in host is RFC 2606 `.invalid`, so a regression that
    /// made one of these *fetchable* still could not reach a real server.
    #[test]
    fn every_way_of_dressing_up_a_foreign_host_is_refused() {
        for url in [
            // a plain foreign host
            "https://evil.example.invalid/texture/1a2b3c",
            // the allowed name as a *prefix* of a foreign domain
            "https://textures.minecraft.net.evil.example.invalid/x",
            // …and as a subdomain of it
            "https://a.textures.minecraft.net.evil.example.invalid/x",
            // the allowed name in the path, where a `contains` check would pass
            "https://evil.example.invalid/textures.minecraft.net",
            // the allowed name as `userinfo`, which is the classic one
            "https://textures.minecraft.net@evil.example.invalid/x",
            "https://textures.minecraft.net:pass@evil.example.invalid/x",
            // a *subdomain* of the allowed host: `ALLOWED_DOMAINS` is exact
            "https://sub.textures.minecraft.net/x",
            // wrong scheme
            "ftp://textures.minecraft.net/x",
            "file:///etc/passwd",
            "data:image/png;base64,AAAA",
            // not a URL at all
            "textures.minecraft.net/x",
            "//textures.minecraft.net/x",
            "",
        ] {
            assert!(
                !is_allowed_texture_domain(url),
                "must refuse {url:?}, which is not the allowed host on an allowed scheme"
            );
        }
    }

    /// The clause that only the bytecode reveals: vanilla compares the host and
    /// the scheme **case-sensitively** and rejects rather than folding. A `Url`
    /// parser normalises both, so an implementation built on the parsed values
    /// alone accepts all four of these — this is the pair that separates the
    /// transcription from the plausible-looking version.
    #[test]
    fn an_unlowered_host_or_scheme_is_refused_not_folded() {
        for url in [
            "https://TEXTURES.MINECRAFT.NET/texture/1a2b3c",
            "https://Textures.Minecraft.Net/texture/1a2b3c",
            "HTTPS://textures.minecraft.net/texture/1a2b3c",
            "Http://textures.minecraft.net/texture/1a2b3c",
        ] {
            // The wrong hypothesis, computed rather than described: an
            // implementation that trusted `Url`'s *normalised* scheme and host
            // accepts every one of these. Asserting that here is what makes the
            // rejection below evidence of the case rule instead of evidence that
            // the URL was malformed for some unrelated reason — no neuter needed,
            // because both hypotheses are evaluated in the same run.
            let parsed = reqwest::Url::parse(url).expect("these are all well-formed URLs");
            assert_eq!(
                parsed.host_str(),
                Some(ALLOWED_TEXTURE_DOMAIN),
                "premise: `Url` lower-cases the host, so the naive check passes {url:?}"
            );
            assert!(
                ALLOWED_TEXTURE_SCHEMES.contains(&parsed.scheme()),
                "premise: `Url` lower-cases the scheme too, so the naive check passes {url:?}"
            );
            assert!(
                !is_allowed_texture_domain(url),
                "vanilla's lowerCaseDomain.equals(decodedDomain) refuses {url:?}"
            );
        }
        // The control: the same URLs already lower-case *are* allowed, so the
        // test above is measuring the case rule and not some other rejection.
        assert!(is_allowed_texture_domain(
            "https://textures.minecraft.net/texture/1a2b3c"
        ));
    }

    /// Our one stricter-than-vanilla rule, asserted so it is a decision rather
    /// than an accident.
    #[test]
    fn a_punycode_host_is_refused_rather_than_decoded() {
        assert!(!is_allowed_texture_domain(
            "https://xn--textures-.xn--minecraft-.xn--net-/x"
        ));
    }

    /// The refusal happens **before** the socket: no allowed host is contacted,
    /// and the stand-in is `.invalid` so even a broken short-circuit could not
    /// reach a real server. `#[tokio::test]` rather than a pure call because the
    /// ordering — check, *then* request — is the property under test.
    #[tokio::test]
    async fn fetch_refuses_a_disallowed_host_without_a_request() {
        crate::install_crypto_provider();
        let client = reqwest::Client::new();
        let err = fetch_texture(&client, "https://evil.example.invalid/skin.png")
            .await
            .expect_err("a foreign host must not be fetched");
        assert!(
            matches!(err, AuthError::TextureDomainNotAllowed { ref url } if url.contains("evil.example.invalid")),
            "expected the typed host refusal, got {err:?}"
        );
    }
}
