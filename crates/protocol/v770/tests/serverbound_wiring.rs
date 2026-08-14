//! The serverbound decode/consume join, gated structurally.
//!
//! # What it is
//!
//! A cross-crate gate asserting that every `ServerBound` variant
//! `lodestone-server` declares is actually **constructed** inside
//! `V770ServerProtocol::decode`. It exists to catch the island class on the
//! serverbound axis, which no `cargo check` at any feature setting can see.
//!
//! # Why it exists
//!
//! `ServerBound` (`lodestone-server`'s `protocol.rs`) and the decode arms that
//! construct it (`lodestone-v770`'s `server_protocol.rs`) live in **different
//! crates**, and nothing in the type system connects them: a variant can be
//! declared, matched by `dispatch_play_packet`, given a fully-written
//! `apply_*` consumer, and **never constructed by any decode arm**. That
//! compiles green, every unit test of the consumer passes, and the packet is
//! silently discarded on the wire forever.
//!
//! It has happened twice, both times from one commit. `c4ad474`
//! ("creative-slot writes, respawn, view-distance and chunk-batch acks now
//! reach a real consumer") added **four** `ServerBound` variants with four
//! consumers, and updated only **two** decode arms. A later investigation
//! found and fixed `ClientInformationChanged` and `ChunkBatchAcknowledged` — while
//! `CreativeModeSlotSet` and `ClientCommand`, from the very same commit,
//! stayed dead for longer still. The user-visible cost of the second one:
//! `apply_client_command`'s `PERFORM_RESPAWN` path was unreachable, so a
//! player who died on a `lodestone` server could never leave the death
//! screen — and a dead player is held on that screen sending **no chunks**,
//! i.e. a permanent silent chunk blackout with keep-alives still flowing.
//!
//! # How it works
//!
//! Two shallow source scans, joined:
//!
//! 1. `declared_variants` reads the `pub enum ServerBound { .. }` block in
//!    `lodestone-server/src/protocol.rs`.
//! 2. `constructed_variants` reads `ServerBound::X` occurrences inside
//!    `V770ServerProtocol::decode`'s body **only**, over source that has had
//!    comments and literals blanked first.
//!
//! Both restrictions are load-bearing, and the first draft of this gate had
//! neither, which made it **half-vacuous** — worth recording because it is
//! the same failure it exists to catch:
//!
//! - Scanning the whole file let a *test assertion* (`assert_eq!(decoded,
//!   ServerBound::Foo { .. })`) count as a construction, so a broken arm
//!   would be masked by its own test.
//! - Scanning raw text let a *comment* count as one. Measured: at the commit
//!   this gate was written against, `server_protocol.rs` contained the
//!   prose `// it up is "add a `ServerBound::CreativeModeSlotSet { slot,` —
//!   and the draft gate reported only `ClientCommand` stranded, silently
//!   missing the second of the two live islands. One comment was the entire
//!   difference between a gate that finds both and a gate that finds half.
//!
//! There is no allowlist and none is needed: the lifecycle variants
//! (`Handshake`, `LoginStart`, `LoginAcknowledged`, `ConfigurationFinished`)
//! are constructed by the handshake/login/config arms and `Ignored` by the
//! wildcard, so the rule is total.
//!
//! # What it does not measure
//!
//! Reachability of the *consumer*, only of the constructor. A variant that
//! decodes and then lands in `dispatch_play_packet`'s no-op group is still
//! stranded, and this gate is silent about that — `cargo xtask connectedness`
//! reports that half, as `decodes-to-Ignored-only`. This is the other
//! direction, which `connectedness` cannot see because it does not know
//! whether a consumer exists.
//!
//! # How to change it
//!
//! Adding a `ServerBound` variant makes this test fail until a decode arm
//! constructs it. That is the intent — fix the arm, do not add an exemption.
//! If a variant genuinely cannot be constructed by `v770` (a future family
//! decoding something `v770` does not), this needs a real per-family split
//! rather than a suppression list.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// Blanks comments and literals, preserving every byte offset and newline.
///
/// Replaces the *contents* of line comments, (nesting) block comments, string
/// literals, raw string literals and char literals with spaces, so a later
/// scan cannot mistake prose or test data for code. Newlines survive so line
/// numbers in any diagnostic stay meaningful.
///
/// # Lifetimes
///
/// `CLAUDE.md` records that this repo's three existing source scanners were
/// all silently broken for months by exactly one thing: a `'` that opens a
/// *lifetime* (`&'static str`) was treated as opening a char literal, and
/// since nothing ever closed it, comment detection was disabled for the rest
/// of the file. The bug never produced a wrong answer visibly — it surfaced
/// only as an unrelated UTF-8 panic in new code.
///
/// So `'` is resolved by **lookahead** rather than by a toggle: it opens a
/// char literal only if what follows is a single character (or a backslash
/// escape) *followed by a closing quote*. Anything else — `'static`, `'a`,
/// `'_` — is a lifetime and is passed through as ordinary code.
/// `the_stripper_survives_a_lifetime_before_a_comment` is the regression gate
/// for precisely that, and it fails if this is reduced to a toggle.
fn blank_comments_and_literals(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(b.len());
    let mut i = 0;

    // Is the `'` at `b[at]` the opening quote of a char literal (rather than a
    // lifetime)? `'x'`, `'\n'`, `'\u{1F600}'` are literals; `'static` is not.
    let is_char_literal = |at: usize| -> Option<usize> {
        // Returns the index just past the closing quote, if this is a literal.
        let mut j = at + 1;
        if j >= b.len() {
            return None;
        }
        if b[j] == '\\' {
            j += 1;
            // Skip the escape body up to the closing quote; `\u{...}` is the
            // longest form, so bound the search rather than parsing it.
            while j < b.len() && b[j] != '\'' && j - at < 12 {
                j += 1;
            }
            return (j < b.len() && b[j] == '\'').then_some(j + 1);
        }
        // A bare single character, then a closing quote.
        (j + 1 < b.len() && b[j + 1] == '\'').then_some(j + 2)
    };

    while i < b.len() {
        let c = b[i];
        let next = b.get(i + 1).copied();

        // Line comment.
        if c == '/' && next == Some('/') {
            while i < b.len() && b[i] != '\n' {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        // Block comment, which nests in Rust.
        if c == '/' && next == Some('*') {
            let mut depth = 0usize;
            while i < b.len() {
                if b[i] == '/' && b.get(i + 1) == Some(&'*') {
                    depth += 1;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                if b[i] == '*' && b.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                    continue;
                }
                out.push(if b[i] == '\n' { '\n' } else { ' ' });
                i += 1;
            }
            continue;
        }
        // Raw string: r"...", r#"..."#, r##"..."##
        if c == 'r' && matches!(next, Some('"') | Some('#')) {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while j < b.len() && b[j] == '#' {
                hashes += 1;
                j += 1;
            }
            if j < b.len() && b[j] == '"' {
                out.extend(std::iter::repeat_n(' ', j - i + 1));
                i = j + 1;
                // Scan for the terminator: `"` followed by `hashes` `#`s.
                while i < b.len() {
                    if b[i] == '"' && b[i + 1..].iter().take(hashes).filter(|c| **c == '#').count() == hashes
                    {
                        out.extend(std::iter::repeat_n(' ', hashes + 1));
                        i += hashes + 1;
                        break;
                    }
                    out.push(if b[i] == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
                continue;
            }
        }
        // Ordinary string literal.
        if c == '"' {
            out.push(' ');
            i += 1;
            while i < b.len() {
                if b[i] == '\\' {
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                if b[i] == '"' {
                    out.push(' ');
                    i += 1;
                    break;
                }
                out.push(if b[i] == '\n' { '\n' } else { ' ' });
                i += 1;
            }
            continue;
        }
        // Char literal, or a lifetime (passed through).
        if c == '\''
            && let Some(end) = is_char_literal(i)
        {
            out.extend(std::iter::repeat_n(' ', end - i));
            i = end;
            continue;
        }
        out.push(c);
        i += 1;
    }
    String::from_iter(out)
}

/// Blanks every `#[cfg(test)]` module body, from already-blanked source.
///
/// Test assertions name `ServerBound` variants constantly
/// (`assert_eq!(decoded, ServerBound::Foo { .. })`), so counting them would let
/// a broken decode arm be masked by its own test — which is exactly what the
/// first draft of this gate did.
///
/// Scoping to `fn decode`'s body instead was tried and is **too narrow**: it
/// reported `ContainerClicked` stranded when it is properly wired, because
/// several arms delegate to a helper that returns `ServerBound` rather than
/// constructing it inline (`cargo xtask connectedness` calls the same thing a
/// "delegate-following classifier"). Excluding tests and keeping every other
/// top-level item is the shape that admits delegates without admitting tests.
///
/// Brace-matching is only safe *because* the input is already blanked — a `{`
/// inside a doc comment or a string would otherwise unbalance it, the same
/// class of bug as the lifetime trap above.
fn blank_test_modules(blanked: &str) -> String {
    let mut out = blanked.to_string();
    let marker = "#[cfg(test)]";
    let mut from = 0usize;
    while let Some(rel) = out[from..].find(marker) {
        let at = from + rel;
        let Some(open_rel) = out[at..].find('{') else { break };
        let open = at + open_rel;
        let mut depth = 0usize;
        let mut close = None;
        for (offset, ch) in out[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + offset + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close.expect("unterminated `#[cfg(test)]` module body");
        let blanked_span: String = out[at..close]
            .chars()
            .map(|c| if c == '\n' { '\n' } else { ' ' })
            .collect();
        out.replace_range(at..close, &blanked_span);
        from = close;
    }
    out
}

/// Variant names declared by the `pub enum ServerBound { .. }` block.
///
/// A shallow scan rather than a Rust parse: variants are the only items at
/// exactly four-space indent inside the block that begin with an uppercase
/// letter and are followed by `{` (struct variant) or `,` (unit variant). Doc
/// comments start with `/`, attributes with `#`, and field lines live at
/// eight-space indent, so none collide.
fn declared_variants(src: &str) -> BTreeSet<String> {
    let open = src
        .find("pub enum ServerBound {")
        .expect("`pub enum ServerBound {` not found in lodestone-server's protocol.rs");
    let body = &src[open..];
    let end = body.find("\n}").expect("unterminated `pub enum ServerBound` block");
    let mut out = BTreeSet::new();
    for line in body[..end].lines().skip(1) {
        let Some(rest) = line.strip_prefix("    ") else {
            continue;
        };
        if rest.starts_with(' ') {
            continue; // a field, at deeper indent
        }
        let name: String = rest.chars().take_while(char::is_ascii_alphanumeric).collect();
        if !name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            continue;
        }
        let tail = rest[name.len()..].trim_start();
        if tail.starts_with('{') || tail.starts_with(',') {
            out.insert(name);
        }
    }
    out
}

/// `ServerBound::X` names occurring in `src`.
fn serverbound_paths(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = src;
    while let Some(at) = rest.find("ServerBound::") {
        rest = &rest[at + "ServerBound::".len()..];
        let name: String = rest.chars().take_while(char::is_ascii_alphanumeric).collect();
        if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            out.insert(name);
        }
    }
    out
}

/// Variants constructed by the crate's real decode path — comments, literals
/// and `#[cfg(test)]` modules all excluded.
///
/// Residual imprecision, stated plainly: this counts any `ServerBound::X` in
/// non-test code, which in `server_protocol.rs` means a construction, since
/// this crate is the *producer* of the enum and never matches on it outside
/// tests. If a future refactor makes it match on `ServerBound` in production
/// code, this gate weakens toward "mentioned somewhere" and would need the
/// delegate-aware scoping that `fn decode`-only scanning attempted.
fn constructed_variants(decode_src: &str) -> BTreeSet<String> {
    serverbound_paths(&blank_test_modules(&blank_comments_and_literals(decode_src)))
}

#[test]
fn every_serverbound_variant_is_constructed_by_decode() {
    let protocol_rs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lodestone-server/src/protocol.rs");
    let enum_src = std::fs::read_to_string(&protocol_rs)
        .unwrap_or_else(|e| panic!("reading {}: {e}", protocol_rs.display()));

    let declared = declared_variants(&enum_src);
    assert!(
        declared.len() > 15,
        "scanner found only {} `ServerBound` variants ({declared:?}) — the enum has far more \
         than that, so the scan itself is broken and this gate is vacuous",
        declared.len()
    );

    let constructed = constructed_variants(include_str!("../src/server_protocol.rs"));
    assert!(
        constructed.contains("Ignored") && constructed.contains("Handshake"),
        "the construction scanner found neither `Ignored` nor `Handshake` inside `fn decode` \
         ({constructed:?}) — it is not reading the decode body, so this gate is vacuous"
    );

    let stranded: Vec<&str> = declared
        .iter()
        .filter(|v| !constructed.contains(*v))
        .map(String::as_str)
        .collect();

    assert!(
        stranded.is_empty(),
        "{} `ServerBound` variant(s) are declared (and consumed) by `lodestone-server` but \
         never constructed inside `V770ServerProtocol::decode`, so the packets that should \
         produce them are silently discarded on the wire: {stranded:?}. This is the island \
         class — see this file's module docs for the two prior instances. Fix the decode arm \
         in `crates/protocol/v770/src/server_protocol.rs`; do not exempt the variant here.",
        stranded.len()
    );
}

/// The detector's own control: the comparison must fail, and name the right
/// variant, when a construction is missing.
///
/// Without this, a scanner that silently returned an empty set would make the
/// gate above pass unconditionally — `CLAUDE.md`'s "assertions of an absence
/// need a control proving the detector works".
#[test]
fn the_wiring_scanners_detect_a_missing_construction() {
    let enum_src = "\
pub enum ServerBound {
    /// A doc comment, which must not be read as a variant.
    #[non_exhaustive]
    Wired {
        /// A field, at deeper indent — also not a variant.
        slot: i16,
    },
    Stranded {
        action: i32,
    },
    Ignored,
}
";
    let declared = declared_variants(enum_src);
    assert_eq!(
        declared.iter().map(String::as_str).collect::<Vec<_>>(),
        ["Ignored", "Stranded", "Wired"],
        "the enum scanner must find exactly the three variants and neither the doc comment, \
         the attribute, nor the two fields"
    );

    let decode_src = "\
fn decode(&self, id: i32) -> ServerBound {
    match id {
        1 => ServerBound::Wired { slot: 0 },
        _ => ServerBound::Ignored,
    }
}
";
    let constructed = constructed_variants(decode_src);
    assert_eq!(
        constructed.iter().map(String::as_str).collect::<Vec<_>>(),
        ["Ignored", "Wired"],
        "the construction scanner must find both constructed variants"
    );

    let stranded: Vec<&String> = declared.iter().filter(|v| !constructed.contains(*v)).collect();
    assert_eq!(
        stranded,
        [&"Stranded".to_string()],
        "the set difference must name the one variant with no construction site — if this is \
         empty, the real gate above cannot fail and is vacuous"
    );
}

/// The exact masking that made this gate's own first draft half-vacuous.
///
/// A `ServerBound::X` written in a **comment** or in a **test assertion** must
/// not count as a construction. Both hypotheses are checked against one
/// input, because the fix for either alone still leaves the gate blind:
/// blanking comments does not exclude tests, and excluding tests does not
/// blank comments.
///
/// The `MaskedByComment` case is not hypothetical — it is
/// a real comment `server_protocol.rs` carried at the commit this gate was
/// written against, and it hid one of the two live islands.
#[test]
fn a_comment_or_a_test_assertion_is_not_a_construction() {
    let decode_src = "\
fn decode(&self, id: i32) -> ServerBound {
    match id {
        // wiring it up is \"add a `ServerBound::MaskedByComment { slot, item }`
        // variant and an arm\" rather than a new feature.
        /* also masked: ServerBound::MaskedByBlockComment */
        1 => ServerBound::Real { slot: 0 },
        _ => ServerBound::Ignored,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn t() {
        assert_eq!(decoded, ServerBound::MaskedByTest { action: 0 });
    }
}
";
    let constructed = constructed_variants(decode_src);
    assert_eq!(
        constructed.iter().map(String::as_str).collect::<Vec<_>>(),
        ["Ignored", "Real"],
        "only the two real constructions may count; a line comment, a block comment and a \
         test assertion each masked an island in the first draft of this gate"
    );
}

/// `CLAUDE.md`'s named trap, as a regression gate.
///
/// This repo's three existing source scanners were all broken by a `'` opening
/// a lifetime and never closing, which disabled comment detection for the
/// remainder of every affected file. A toggle-based stripper fails this: the
/// `'static` would swallow everything up to the next `'`, leaving the comment
/// on the following line unblanked and `Masked` counted as a construction.
#[test]
fn the_stripper_survives_a_lifetime_before_a_comment() {
    let decode_src = "\
fn decode(&self, name: &'static str, c: char) -> ServerBound {
    let _ = 'a';
    let _ = '\\'';
    let _ = '\\u{1F600}';
    // ServerBound::Masked
    ServerBound::Ignored
}
";
    let constructed = constructed_variants(decode_src);
    assert_eq!(
        constructed.iter().map(String::as_str).collect::<Vec<_>>(),
        ["Ignored"],
        "a lifetime (`&'static str`) before a comment must not disable comment blanking — \
         this is the exact bug that silently broke three scanners in this repo"
    );

    // And the lifetime itself must survive as code, not be eaten as a literal:
    // if `'static str` were blanked, the `fn decode(` body scan would still
    // work but the stripper would be corrupting real source.
    let blanked = blank_comments_and_literals("fn f(x: &'static str) -> &'a T { 'a: loop {} }");
    assert!(
        blanked.contains("'static") && blanked.contains("'a"),
        "lifetimes must pass through as code, got: {blanked:?}"
    );
}
