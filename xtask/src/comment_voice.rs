//! `cargo xtask check-comment-voice` — a text-based guard against comments
//! written in the voice of the change that introduced them, and against
//! issue references left as the only explanation for *why* code looks the
//! way it does.
//!
//! # Why this exists
//!
//! An issue reference sends the reader somewhere else to learn what the
//! code in front of them does. "This change" (or "this commit"/"this
//! patch"/"this PR") names nothing once committed — a doc comment is not a
//! pull request description. Both read as authoritative long after they
//! stop being accurate, because both were true the moment they were
//! written. `CLAUDE.md` records the fix for exactly this shape of rot:
//! *whenever the type system cannot express a constraint, make it
//! checkable and check it.* A comment asserting an invariant is
//! documentation of intent, not a guard — this scanner is the guard.
//!
//! # Scope: comments and doc comments only, never code or string content
//!
//! This is a text scanner, not a `syn`-based one (unlike [`crate::islands`]
//! and [`crate::ptr_const`]): a `.md` file has no AST, and a WGSL shader's
//! only Rust-adjacent tooling in this workspace is `naga`, which discards
//! comments during validation. So each of the three extensions in scope
//! (`.rs`, `.md`, `.wgsl`) gets its own hand-rolled "mask everything that
//! is not commentary" pass, and the two patterns below are matched only
//! against what survives:
//!
//! - **`.rs`**: `//` line comments (this also covers `///`/`//!` doc
//!   comments, which are lexically just `//` with an extra leading `/` or
//!   `!`) and `/* */` block comments (nested, per Rust's actual grammar),
//!   with string/byte-string/raw-string and char/byte-char literals
//!   tracked and excluded so a `//` or `#123`-shaped substring inside a
//!   string is never mistaken for a comment or a hit. A bare `'` that does
//!   not resolve to a complete char literal is left alone rather than
//!   guessed at — that guess is exactly what CLAUDE.md's `ptr_const`
//!   module doc warns cost three earlier scanners here their correctness,
//!   because `&'static str` opens what looks like an unterminated char
//!   literal. Two known, accepted blind spots from that trade: a `\x`/`\u`
//!   escape is skipped by its own width rather than fully validated, and a
//!   char literal spelled with a raw `"` (`'"'`) is recognised, but one
//!   spelled as an *unrecognised* escape sequence is not — either can, in
//!   the rarest case, desynchronise the mask for a few characters. Neither
//!   has been observed in this workspace's real source.
//! - **`.wgsl`**: `//` and `/* */` only — WGSL's grammar (as used in this
//!   workspace's shaders) has no string or char literal to misdetect a
//!   comment delimiter inside, so no literal-tracking is needed.
//! - **`.md`**: the opposite framing — nearly all prose in a doc *is*
//!   commentary, so this mask instead blanks out fenced code blocks
//!   (`` ``` `` / `~~~`) and lets everything else through. A fenced sample
//!   is a literal artifact (output, a config snippet), not narration about
//!   the repo, so it is out of scope the same way a `.rs` string literal
//!   is.
//!
//! # The two patterns, and their false-positive traps
//!
//! 1. **Issue references** — `#123`-shaped. A naive `#\d+` substring catches
//!    three things it must not: `#[derive(...)]` (needs a digit right after
//!    `#`, which `[` never is — excluded structurally, no boundary check
//!    needed), a hex colour literal like `#1a2b3c` (excluded by requiring
//!    the character *after* the digit run to not itself be alphanumeric —
//!    `#1` followed by `a` fails that check), and a same-page or
//!    cross-document URL fragment like `guide.md#123-notes` (excluded by
//!    requiring the character *before* `#` to not be a path-shaped
//!    character — alphanumeric, `.`, `/`, `-`, `_`, or `#` itself; a real
//!    issue reference is always preceded by whitespace or an opening
//!    delimiter like `(`).
//! 2. **Change-voice phrases** — `this change`, `this commit`, `this
//!    patch`, `before this change`, `this PR`; case-insensitive, and
//!    **word-bounded on both ends**. This is the trap named in the issue
//!    that added this scanner: a case-insensitive *substring* search for
//!    "this pr" matches inside "this **pr**ocess" and "this **pr**operty"
//!    — measured at 431 false hits across this workspace against a true
//!    count of zero. Word-bounding on the character after the match (not
//!    alphanumeric/`_`) is what removes every one of them; `this process`
//!    stops at `pr` followed by `o`, which is alphanumeric, so it never
//!    matches. `before this change` is checked first and suppresses the
//!    `this change` match nested inside it, so one comment produces one
//!    finding, not two.
//!
//! # Deliberately out of scope
//!
//! `used to be` is not a pattern here — see the issue that added this
//! scanner: it is frequently a fact about data on disk ("the field used to
//! be one byte, so old saves carry the narrow form"), not changelog
//! narration, and needs judgement a pattern ban cannot supply. Commit
//! messages and issue/PR comments are never read by this scanner — it only
//! walks `.rs`/`.md`/`.wgsl` file content.
//!
//! # Allowlist
//!
//! Follows [`crate::DEFAULT_CONNECTED_ALLOWLIST`]'s precedent
//! (`xtask/check-connected.toml`): [`DEFAULT_ALLOWLIST`]
//! (`xtask/check-comment-voice.toml`) is a `[[allow]]`-entry TOML-subset
//! file, each entry requiring a non-empty `owner` and `reason` so an
//! exception is a recorded decision. An entry names a `file` (relative to
//! the workspace root) and may optionally narrow it, either by `text` (a
//! substring of the masked comment) or by `line`. **Prefer `text`**: a line
//! number breaks whenever anything above it moves, which in a checkout
//! several agents edit at once means the guard fails on hits it was told to
//! allow. Omitting both allows every hit in that file. This is deliberately coarser than
//! `check-ptr-const`'s per-call-site precision, because the volume this
//! scanner starts from (thousands of pre-existing issue references) is
//! cleared file-by-file and crate-by-crate, not line-by-line in one pass —
//! see the issue that added this scanner.

use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Default allowlist path, relative to the workspace root — mirrors
/// [`crate::DEFAULT_CONNECTED_ALLOWLIST`]'s naming.
pub const DEFAULT_ALLOWLIST: &str = "xtask/check-comment-voice.toml";

/// Directory names pruned at any depth. `target` keeps a shared checkout's
/// build output out of the walk (as in [`crate::ptr_const`]); `vendor`
/// excludes the nested third-party `minecraft-data` checkout, which is not
/// this repository's own commentary and carries its own `.git`.
const EXCLUDED_DIR_NAMES: &[&str] = &[
    "target",
    ".git",
    ".cache",
    ".worktrees",
    "node_modules",
    "vendor",
    ".jj",
];

/// A sanity floor on how many `.rs`/`.md`/`.wgsl` files the walk must find,
/// so a moved directory or a bad exclusion reads as "the walk is broken",
/// never as "nothing to scan". Measured at 2007 files workspace-wide the
/// day this guard was added (excluding the dirs above); set well under
/// that, mirroring `check-ptr-const`'s `MIN_FILES_SCANNED`.
const MIN_FILES_SCANNED: usize = 1500;

/// This scanner's own source, excluded from its own scan.
///
/// A detector necessarily contains every pattern it detects: the file that
/// knows to flag `this PR` has to spell `this PR`. Allowlisting each such
/// line instead was tried first and cost ten entries that all broke the
/// moment anything above them moved. `scripts/clean-room-census.py` excludes
/// itself for exactly this reason and says so in its own header.
///
/// The cost is real and worth naming: a genuine comment-voice defect in this
/// one file goes unseen. That is preferable to a guard whose self-entries
/// rot, because a guard that reports false failures gets switched off.
const SELF_SOURCE: &str = "xtask/src/comment_voice.rs";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PatternKind {
    IssueReference,
    BeforeThisChange,
    ThisChange,
    ThisCommit,
    ThisPatch,
    ThisPr,
    VanillaGoalName,
    VanillaClassName,
    VanillaMemberName,
}

impl PatternKind {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            PatternKind::IssueReference => "issue reference",
            PatternKind::BeforeThisChange => "\"before this change\"",
            PatternKind::ThisChange => "\"this change\"",
            PatternKind::ThisCommit => "\"this commit\"",
            PatternKind::ThisPatch => "\"this patch\"",
            PatternKind::ThisPr => "\"this PR\"",
            PatternKind::VanillaGoalName => "vanilla goal-class name",
            PatternKind::VanillaClassName => "vanilla class name",
            PatternKind::VanillaMemberName => "vanilla method or field name",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AllowedBy {
    pub owner: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct Hit {
    /// Path relative to the workspace root, `/`-separated.
    pub file: String,
    /// 1-based line number.
    pub line: usize,
    pub kind: PatternKind,
    /// The comment-only (masked) line, trimmed — code on the same line
    /// reads as blank space, which is itself informative in the census.
    pub snippet: String,
    pub allowed: Option<AllowedBy>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub files_scanned: usize,
    pub rs_files: usize,
    pub md_files: usize,
    pub wgsl_files: usize,
    pub hits: Vec<Hit>,
    /// Allowlist entries that matched zero hits this run — a stale
    /// exception, printed as a hint (not a failure) so shrinking the
    /// allowlist file-by-file has a signal to work from.
    pub stale_allow_entries: Vec<String>,
}

impl Report {
    #[must_use]
    pub fn violations(&self) -> Vec<&Hit> {
        self.hits.iter().filter(|h| h.allowed.is_none()).collect()
    }
}

// ---------------------------------------------------------------------
// Allowlist
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
struct AllowEntry {
    file: String,
    line: Option<usize>,
    /// Matches on the hit's masked comment text instead of its line number.
    ///
    /// A line number is a poor key in a shared checkout: any edit ABOVE a
    /// pinned line silently invalidates the entry, and the guard then fails
    /// on a hit it was told to allow. That happened the first day this
    /// scanner ran -- two entries pinned at 8281/10844 broke when an
    /// unrelated change ten lines earlier pushed the same comments to
    /// 8291/10854. A guard that cries wolf gets switched off, so prefer
    /// `text`; it survives arbitrary movement within the file.
    text: Option<String>,
    owner: String,
    reason: String,
}

fn parse_allowlist(contents: &str) -> Result<Vec<AllowEntry>> {
    #[derive(Default)]
    struct Builder {
        file: Option<String>,
        line: Option<usize>,
        text: Option<String>,
        owner: Option<String>,
        reason: Option<String>,
    }

    fn finish(builder: Builder, index: usize, out: &mut Vec<AllowEntry>) -> Result<()> {
        let file = builder.file.unwrap_or_default();
        let owner = builder.owner.unwrap_or_default();
        let reason = builder.reason.unwrap_or_default();
        let mut missing = Vec::new();
        if file.trim().is_empty() {
            missing.push("file");
        }
        if owner.trim().is_empty() {
            missing.push("owner");
        }
        if reason.trim().is_empty() {
            missing.push("reason");
        }
        if !missing.is_empty() {
            bail!(
                "allow entry {index} is missing non-empty {}",
                missing.join(", ")
            );
        }
        out.push(AllowEntry {
            file,
            line: builder.line,
            text: builder.text,
            owner,
            reason,
        });
        Ok(())
    }

    let mut out = Vec::new();
    let mut current: Option<Builder> = None;
    let mut entry_index = 0usize;
    for raw_line in contents.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "[[allow]]" {
            if let Some(builder) = current.take() {
                finish(builder, entry_index, &mut out)?;
            }
            entry_index += 1;
            current = Some(Builder::default());
            continue;
        }
        let Some(builder) = current.as_mut() else {
            bail!("allowlist entries must start with [[allow]]");
        };
        let (key, raw_value) = trimmed
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("expected key = value, got {trimmed:?}"))?;
        let key = key.trim();
        let raw_value = raw_value.trim();
        match key {
            "file" => builder.file = Some(parse_toml_string(raw_value)?),
            "owner" => builder.owner = Some(parse_toml_string(raw_value)?),
            "reason" => builder.reason = Some(parse_toml_string(raw_value)?),
            "text" => builder.text = Some(parse_toml_string(raw_value)?),
            "line" => {
                let n: usize = raw_value
                    .parse()
                    .with_context(|| format!("expected integer for `line`, got {raw_value:?}"))?;
                builder.line = Some(n);
            }
            other => bail!("unsupported check-comment-voice allowlist key {other:?}"),
        }
    }
    if let Some(builder) = current {
        finish(builder, entry_index, &mut out)?;
    }

    let mut seen = BTreeSet::new();
    for entry in &out {
        let key = (entry.file.clone(), entry.line, entry.text.clone());
        if !seen.insert(key) {
            bail!(
                "duplicate check-comment-voice allowlist entry for {:?} line {:?}",
                entry.file,
                entry.line
            );
        }
    }
    Ok(out)
}

fn parse_toml_string(raw: &str) -> Result<String> {
    let Some(value) = raw.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        bail!("expected a quoted string, got {raw:?}");
    };
    Ok(value.to_owned())
}

fn load_allowlist(workspace_root: &Path, allowlist_path: &Path) -> Result<Vec<AllowEntry>> {
    let path = if allowlist_path.is_absolute() {
        allowlist_path.to_owned()
    } else {
        workspace_root.join(allowlist_path)
    };
    let contents =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    parse_allowlist(&contents).map_err(|error| anyhow::anyhow!("parse {}: {error:#}", path.display()))
}

// ---------------------------------------------------------------------
// File walk
// ---------------------------------------------------------------------

fn collect_scan_files(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    walk(workspace_root, &mut files)?;
    files.sort();
    Ok(files)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // A shared checkout: a directory can vanish between the parent's
        // listing and this recursive call landing on it. Nothing to scan
        // there is not a defect in the scan -- mirrors `ptr_const`'s walker.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("read dir {}", dir.display())),
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("read dir entry under {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("stat {}", path.display()))?;
        if file_type.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if EXCLUDED_DIR_NAMES.contains(&name) {
                    continue;
                }
            }
            walk(&path, out)?;
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if matches!(ext, "rs" | "md" | "wgsl") {
                out.push(path);
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Rust,
    Markdown,
    Wgsl,
}

fn file_kind(path: &Path) -> Option<FileKind> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => Some(FileKind::Rust),
        Some("md") => Some(FileKind::Markdown),
        Some("wgsl") => Some(FileKind::Wgsl),
        _ => None,
    }
}

// ---------------------------------------------------------------------
// Comment/prose masking -- see the module doc for the design rationale.
// ---------------------------------------------------------------------

/// Attempts to parse a string-literal-shaped span (`"..."`, `b"..."`,
/// `r"..."`/`r#"..."#`/.., `br"..."`/`br#"..."#`/..) starting at `start`.
/// Returns the exclusive end index on success. A `\x`/`\u{..}` escape is
/// skipped by its expected width rather than fully validated -- see the
/// module doc's accepted-blind-spot note.
fn consume_string_like(chars: &[char], start: usize) -> Option<usize> {
    let n = chars.len();
    let mut i = start;
    if chars.get(i) == Some(&'b') {
        i += 1;
    }
    if chars.get(i) == Some(&'r') {
        let mut j = i + 1;
        let mut hashes = 0usize;
        while chars.get(j) == Some(&'#') {
            hashes += 1;
            j += 1;
        }
        if chars.get(j) != Some(&'"') {
            // Not actually a raw string -- e.g. a raw identifier `r#type`,
            // or a bare `r`/`br` identifier. Not this scanner's concern.
            return None;
        }
        let mut k = j + 1;
        loop {
            if k >= n {
                return Some(n);
            }
            if chars[k] == '"' {
                let mut h = 0usize;
                let mut m = k + 1;
                while h < hashes && chars.get(m) == Some(&'#') {
                    h += 1;
                    m += 1;
                }
                if h == hashes {
                    return Some(m);
                }
            }
            k += 1;
        }
    }
    if chars.get(i) == Some(&'"') {
        return consume_plain_string(chars, i);
    }
    None
}

fn consume_plain_string(chars: &[char], quote_pos: usize) -> Option<usize> {
    let n = chars.len();
    let mut k = quote_pos + 1;
    while k < n {
        match chars[k] {
            '\\' => {
                k = skip_escape(chars, k);
            }
            '"' => return Some(k + 1),
            _ => k += 1,
        }
    }
    Some(n)
}

/// `chars[at]` is the backslash. Returns the index just past the escape
/// sequence.
fn skip_escape(chars: &[char], at: usize) -> usize {
    let n = chars.len();
    match chars.get(at + 1) {
        Some('x') => (at + 4).min(n),
        Some('u') => {
            let mut m = at + 2;
            if chars.get(m) == Some(&'{') {
                m += 1;
                while m < n && chars[m] != '}' {
                    m += 1;
                }
                if m < n {
                    m += 1;
                }
                m
            } else {
                (at + 2).min(n)
            }
        }
        Some(_) => (at + 2).min(n),
        None => n,
    }
}

/// `chars[start]` must be `'`. Returns the exclusive end index if this
/// resolves to a complete char literal (simple `'x'` or a recognised
/// escape), `None` if it looks like a lifetime or a stray quote -- see the
/// module doc for why an unresolved case is left alone rather than guessed
/// at.
fn consume_char_literal(chars: &[char], start: usize) -> Option<usize> {
    let n = chars.len();
    if chars.get(start + 1) == Some(&'\\') {
        let end = skip_escape(chars, start + 1);
        if chars.get(end) == Some(&'\'') {
            return Some(end + 1);
        }
        return None;
    }
    if start + 2 < n && chars[start + 2] == '\'' {
        return Some(start + 3);
    }
    None
}

/// Which characters of a `.rs` file are comment text and which are string or
/// char literal content; everything else is code.
struct RustRegions {
    chars: Vec<char>,
    comment: Vec<bool>,
    literal: Vec<bool>,
}

fn lex_rust(src: &str) -> RustRegions {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut comment = vec![false; n];
    let mut literal = vec![false; n];
    let mut i = 0usize;
    while i < n {
        let c = chars[i];
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < n && chars[i] != '\n' {
                comment[i] = true;
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            comment[i] = true;
            comment[i + 1] = true;
            i += 2;
            let mut depth = 1u32;
            while i < n && depth > 0 {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    comment[i] = true;
                    comment[i + 1] = true;
                    depth += 1;
                    i += 2;
                    continue;
                }
                if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    comment[i] = true;
                    comment[i + 1] = true;
                    depth -= 1;
                    i += 2;
                    continue;
                }
                comment[i] = true;
                i += 1;
            }
            continue;
        }
        if let Some(end) = consume_string_like(&chars, i) {
            literal[i..end].fill(true);
            i = end;
            continue;
        }
        if c == '\'' {
            if let Some(end) = consume_char_literal(&chars, i) {
                literal[i..end].fill(true);
                i = end;
                continue;
            }
        }
        i += 1;
    }
    RustRegions { chars, comment, literal }
}

/// Masks `src` to comment-only text for a `.rs` file: every character that
/// is not inside a `//`/`/* */` comment becomes `' '`, with string/char
/// literal content tracked (but not itself preserved) so a `//`/`"`
/// sequence inside one is never mistaken for a comment or string
/// delimiter. Newlines are always preserved so line numbers stay aligned.
fn mask_to_rust_comments(src: &str) -> String {
    let regions = lex_rust(src);
    regions
        .chars
        .iter()
        .zip(&regions.comment)
        .map(|(&c, &in_comment)| if c == '\n' || in_comment { c } else { ' ' })
        .collect()
}

/// The code of a `.rs` file with comments and literals blanked out.
fn rust_code_only(src: &str) -> String {
    let regions = lex_rust(src);
    regions
        .chars
        .iter()
        .zip(regions.comment.iter().zip(&regions.literal))
        .map(|(&c, (&in_comment, &in_literal))| if c == '\n' || !(in_comment || in_literal) { c } else { ' ' })
        .collect()
}

/// Masks `src` to comment-only text for a `.wgsl` file: `//` and `/* */`
/// only -- see the module doc for why no literal-tracking is needed here.
fn mask_to_wgsl_comments(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut out = vec![' '; n];
    let mut i = 0usize;
    while i < n {
        let c = chars[i];
        if c == '\n' {
            out[i] = '\n';
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < n && chars[i] != '\n' {
                out[i] = chars[i];
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            out[i] = '/';
            out[i + 1] = '*';
            i += 2;
            let mut depth = 1u32;
            while i < n && depth > 0 {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    out[i] = '/';
                    out[i + 1] = '*';
                    depth += 1;
                    i += 2;
                    continue;
                }
                if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    out[i] = '*';
                    out[i + 1] = '/';
                    depth -= 1;
                    i += 2;
                    continue;
                }
                out[i] = chars[i];
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    out.into_iter().collect()
}

/// Masks `src` to prose-only text for a `.md` file: fenced code blocks
/// (`` ``` `` or `~~~`, any length >= 3) are blanked; everything else
/// passes through unchanged, since nearly all Markdown prose in this repo
/// is commentary about the code, not code itself.
fn mask_to_markdown_prose(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut fence: Option<(char, usize)> = None;
    for line in src.split_inclusive('\n') {
        let (content, newline) = match line.strip_suffix('\n') {
            Some(c) => (c, true),
            None => (line, false),
        };
        let blank = |s: &str| " ".repeat(s.chars().count());
        let trimmed = content.trim_start();
        if let Some((marker_char, marker_len)) = fence {
            out.push_str(&blank(content));
            if newline {
                out.push('\n');
            }
            let run = trimmed.chars().take_while(|&c| c == marker_char).count();
            if run >= marker_len && trimmed[run.min(trimmed.len())..].trim().is_empty() && run > 0
            {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let marker_char = trimmed.chars().next().expect("checked starts_with above");
            let marker_len = trimmed.chars().take_while(|&c| c == marker_char).count();
            fence = Some((marker_char, marker_len));
            out.push_str(&blank(content));
            if newline {
                out.push('\n');
            }
            continue;
        }
        out.push_str(content);
        if newline {
            out.push('\n');
        }
    }
    out
}

// ---------------------------------------------------------------------
// Pattern matching over masked (comment/prose-only) text
// ---------------------------------------------------------------------

/// Finds every `#123`-shaped issue reference in `line`. See the module doc
/// for exactly what this excludes and why.
fn find_issue_references(line: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut hits = Vec::new();
    let mut i = 0usize;
    while i < n {
        if chars[i] == '#' {
            let prev_ok = if i == 0 {
                true
            } else {
                let p = chars[i - 1];
                !(p.is_alphanumeric() || matches!(p, '_' | '.' | '/' | '-' | '#'))
            };
            let mut j = i + 1;
            while j < n && chars[j].is_ascii_digit() {
                j += 1;
            }
            let digit_count = j - (i + 1);
            if digit_count > 0 && prev_ok {
                let next_ok = if j >= n {
                    true
                } else {
                    let c = chars[j];
                    !(c.is_alphanumeric() || c == '_')
                };
                if next_ok {
                    hits.push((i, j));
                    i = j;
                    continue;
                }
            }
        }
        i += 1;
    }
    hits
}

/// Finds every word-bounded, case-insensitive occurrence of `needle` (given
/// already lowercase) in `haystack`. Word-bounded on *both* ends is what
/// keeps "this PR" out of "this process"/"this property" -- see the module
/// doc.
fn find_word_bounded(haystack: &str, needle_lower: &str) -> Vec<(usize, usize)> {
    let hay: Vec<char> = haystack.chars().collect();
    let needle: Vec<char> = needle_lower.chars().collect();
    let mut hits = Vec::new();
    if needle.is_empty() || hay.len() < needle.len() {
        return hits;
    }
    for start in 0..=(hay.len() - needle.len()) {
        let end = start + needle.len();
        let matches = hay[start..end]
            .iter()
            .zip(&needle)
            .all(|(a, b)| a.to_ascii_lowercase() == *b || a.to_lowercase().eq(b.to_lowercase()));
        if !matches {
            continue;
        }
        let before_ok = start == 0
            || !(hay[start - 1].is_alphanumeric() || hay[start - 1] == '_');
        let after_ok = end == hay.len() || !(hay[end].is_alphanumeric() || hay[end] == '_');
        if before_ok && after_ok {
            hits.push((start, end));
        }
    }
    hits
}

fn overlaps(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// Scans one already-masked line, returning `(kind, char_start, char_end)`
/// triples. `before this change` is checked first so its nested `this
/// change` is suppressed -- one comment, one finding.
fn scan_line(masked_line: &str, names: &ReferenceNames, bare_members: bool) -> Vec<(PatternKind, usize, usize)> {
    let lower = masked_line.to_lowercase();
    let mut out = Vec::new();

    let before_change = find_word_bounded(&lower, "before this change");
    for span in &before_change {
        out.push((PatternKind::BeforeThisChange, span.0, span.1));
    }
    for span in find_word_bounded(&lower, "this change") {
        if before_change.iter().any(|b| overlaps(*b, span)) {
            continue;
        }
        out.push((PatternKind::ThisChange, span.0, span.1));
    }
    for span in find_word_bounded(&lower, "this commit") {
        out.push((PatternKind::ThisCommit, span.0, span.1));
    }
    for span in find_word_bounded(&lower, "this patch") {
        out.push((PatternKind::ThisPatch, span.0, span.1));
    }
    for span in find_word_bounded(&lower, "this pr") {
        out.push((PatternKind::ThisPr, span.0, span.1));
    }
    for span in find_issue_references(masked_line) {
        out.push((PatternKind::IssueReference, span.0, span.1));
    }
    for name in &names.goals {
        for span in find_word_bounded(masked_line, name) {
            out.push((PatternKind::VanillaGoalName, span.0, span.1));
        }
    }
    if !names.all_classes.is_empty() || !names.members.is_empty() {
        let chars: Vec<char> = masked_line.chars().collect();
        let mut start = 0;
        while start < chars.len() {
            if !(chars[start].is_alphanumeric() || chars[start] == '_') {
                start += 1;
                continue;
            }
            let mut end = start;
            while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            let token: String = chars[start..end].iter().collect();
            if names.classes.contains(&token) {
                out.push((PatternKind::VanillaClassName, start, end));
            } else if (names.members.contains(&token) && ((bare_members && is_camel_case(&token)) || is_member_use(&chars, start, end)))
                || (bare_members && is_reference_constant(&chars, start, &token, names))
            {
                out.push((PatternKind::VanillaMemberName, start, end));
            }
            start = end;
        }
    }
    out.sort_by_key(|(_, start, _)| *start);
    out
}

/// Whether `token` is camelCase with an interior capital (`setBlockState`, not
/// `tick`): such a listed name is a citation even when written bare, since a
/// data key spelled the same way is exempted by [`own_member_tokens`].
fn is_camel_case(token: &str) -> bool {
    token.chars().next().is_some_and(char::is_lowercase) && token.chars().any(char::is_uppercase)
}

/// Whether the token at `chars[start..]` is a Java-style constant reached
/// through a reference class (`Class.UPPER_CASE`). The class name is checked
/// against the full reference list, since our own type may share its name.
fn is_reference_constant(chars: &[char], start: usize, token: &str, names: &ReferenceNames) -> bool {
    if token.len() < 2 || !token.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
        return false;
    }
    let Some(dot) = start.checked_sub(1).filter(|&i| chars[i] == '.') else { return false };
    let mut class_start = dot;
    while class_start > 0 && (chars[class_start - 1].is_alphanumeric() || chars[class_start - 1] == '_') {
        class_start -= 1;
    }
    let class: String = chars[class_start..dot].iter().collect();
    names.all_classes.contains(&class)
}

/// Whether the identifier at `chars[start..end]` is written the way a method
/// or field of another codebase is cited: called (`name(`), or reached through
/// a type or receiver (`Type.name`, `Type::name`, `Type#name`).
fn is_member_use(chars: &[char], start: usize, end: usize) -> bool {
    if chars.get(end) == Some(&'(') {
        return true;
    }
    let before = &chars[..start];
    let qualified = match before {
        [.., ':', ':'] | [.., '#'] => true,
        [.., '.'] => before.len() >= 2 && (before[before.len() - 2].is_alphanumeric() || before[before.len() - 2] == '_'),
        _ => false,
    };
    qualified
}

/// Paths whose comments still spell bare camelCase reference members or
/// `Class.CONSTANT` forms, and so are checked only for the called or qualified
/// member forms until each is rewritten. Only shrinks:
/// a path absent here is held to the bare rule.
const BARE_MEMBER_PENDING: &[&str] = &[
    "crates/lodestone-assets/",
    "crates/lodestone-entity/",
    "crates/lodestone-game/",
    "crates/lodestone-physics/",
    "crates/lodestone-render/",
    "crates/lodestone-server/",
    "crates/lodestone-shell/",
    "crates/lodestone-worldgen/",
    "crates/versions/",
];

/// Where the goal-name table lives; the only file allowed to spell a
/// reference goal-class name.
const GOAL_NAMES_PATH: &str = "crates/lodestone-entity/data/goal-names.tsv";

/// The committed list of reference class names (one per line), generated by
/// `scripts/gen-vanilla-class-names.py`. Absent in a fixture tree, which then
/// scans for goal names only.
const CLASS_NAMES_PATH: &str = "xtask/vanilla-class-names.txt";

/// The committed list of reference method and field names (one per line),
/// generated by `scripts/gen-vanilla-member-names.py`. A name is flagged only
/// where a comment calls or qualifies it (see [`is_member_use`]) and this
/// workspace's own code never spells it.
const MEMBER_NAMES_PATH: &str = "xtask/vanilla-member-names.txt";

/// Member names that also belong to a third-party API we interoperate with
/// (one per line, `#` comments), subtracted from [`MEMBER_NAMES_PATH`].
const THIRD_PARTY_MEMBER_NAMES_PATH: &str = "xtask/third-party-member-names.txt";

/// Reference names a comment or doc must not spell.
#[derive(Debug, Default)]
struct ReferenceNames {
    /// Goal-class names from the goal-name table, lowercased for the
    /// case-insensitive [`find_word_bounded`].
    goals: Vec<String>,
    /// Reference class names matched case-sensitively as whole identifiers.
    classes: BTreeSet<String>,
    /// Reference method and field names, matched like [`Self::classes`] but
    /// only in a cited or camelCase form.
    members: BTreeSet<String>,
    /// Every reference class name, including those our own code also uses;
    /// qualifies a Java-style constant.
    all_classes: BTreeSet<String>,
}

/// Identifiers that appear in this workspace's own code: a reference class
/// name used there is ours to name, so comments about it are not flagged.
fn own_code_identifiers(rust_sources: &[String]) -> BTreeSet<String> {
    let mut own = BTreeSet::new();
    for src in rust_sources {
        let code = rust_code_only(src);
        let mut token = String::new();
        for c in code.chars().chain(std::iter::once(' ')) {
            if c.is_alphanumeric() || c == '_' {
                token.push(c);
            } else if !token.is_empty() {
                if token.chars().next().is_some_and(char::is_uppercase) {
                    own.insert(std::mem::take(&mut token));
                } else {
                    token.clear();
                }
            }
        }
    }
    own
}

/// camelCase identifiers this workspace's own code or string literals spell:
/// data keys such as an NBT or JSON field are ours to name.
fn own_member_tokens(rust_sources: &[String]) -> BTreeSet<String> {
    let mut own = BTreeSet::new();
    for src in rust_sources {
        let regions = lex_rust(src);
        let mut token = String::new();
        for i in 0..=regions.chars.len() {
            let (c, in_comment) = match regions.chars.get(i) {
                Some(&c) => (c, regions.comment[i]),
                None => (' ', false),
            };
            if !in_comment && (c.is_alphanumeric() || c == '_') {
                token.push(c);
            } else if !token.is_empty() {
                let t = std::mem::take(&mut token);
                if t.chars().next().is_some_and(char::is_lowercase) && t.chars().any(char::is_uppercase) {
                    own.insert(t);
                }
            }
        }
    }
    own
}

fn reference_names(workspace_root: &Path, rust_sources: &[String]) -> ReferenceNames {
    let own = own_code_identifiers(rust_sources);
    let own_members = own_member_tokens(rust_sources);
    let mut goals = BTreeSet::new();
    if let Ok(table) = std::fs::read_to_string(workspace_root.join(GOAL_NAMES_PATH)) {
        for line in table.lines() {
            let Some((_, reference)) = line.split_once('\t') else { continue };
            let reference = reference.split('(').next().unwrap_or("");
            for token in reference.split('.') {
                if token.len() > 4
                    && token.ends_with("Goal")
                    && token.chars().all(|c| c.is_ascii_alphanumeric())
                    && !own.contains(token)
                {
                    goals.insert(token.to_lowercase());
                }
            }
        }
    }
    let all_classes: BTreeSet<String> = std::fs::read_to_string(workspace_root.join(CLASS_NAMES_PATH))
        .map(|list| list.lines().map(str::trim).filter(|name| !name.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default();
    let classes = all_classes.iter().filter(|name| !own.contains(*name)).cloned().collect();
    let third_party = std::fs::read_to_string(workspace_root.join(THIRD_PARTY_MEMBER_NAMES_PATH)).unwrap_or_default();
    let third_party: BTreeSet<&str> = third_party.lines().map(str::trim).filter(|l| !l.starts_with('#')).collect();
    let members = std::fs::read_to_string(workspace_root.join(MEMBER_NAMES_PATH))
        .map(|list| {
            list.lines()
                .map(str::trim)
                .filter(|name| !name.is_empty() && !own_members.contains(*name) && !third_party.contains(*name))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    ReferenceNames { goals: goals.into_iter().collect(), classes, members, all_classes }
}

// ---------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------

fn scan_file(rel_path: &str, kind: FileKind, src: &str, names: &ReferenceNames) -> Vec<Hit> {
    let masked = match kind {
        FileKind::Rust => mask_to_rust_comments(src),
        FileKind::Wgsl => mask_to_wgsl_comments(src),
        FileKind::Markdown => mask_to_markdown_prose(src),
    };
    let bare_members = !BARE_MEMBER_PENDING.iter().any(|prefix| rel_path.starts_with(prefix));
    let mut hits = Vec::new();
    for (idx, line) in masked.split('\n').enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        for (kind, start, end) in scan_line(line, names, bare_members) {
            let snippet: String = line.chars().skip(start.saturating_sub(20)).take(80).collect();
            hits.push(Hit {
                file: rel_path.to_owned(),
                line: idx + 1,
                kind,
                snippet: snippet.trim().to_owned(),
                allowed: None,
            });
            let _ = end; // used only to compute the snippet window above
        }
    }
    hits
}

/// The scan itself, given an already-collected file list. Split from
/// [`scan_workspace`] so a unit test can drive a small fixture tree
/// without tripping the file-count floor, mirroring `check-ptr-const`.
fn scan_paths(files: &[PathBuf], workspace_root: &Path) -> Result<Report> {
    let mut report = Report::default();
    let mut loaded: Vec<(String, FileKind, String)> = Vec::new();
    for path in files {
        let Some(kind) = file_kind(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(workspace_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if rel == SELF_SOURCE {
            continue;
        }
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            // A shared checkout: tolerate a file vanishing mid-walk, as
            // `walk` above already does for a directory.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
        };
        report.files_scanned += 1;
        match kind {
            FileKind::Rust => report.rs_files += 1,
            FileKind::Markdown => report.md_files += 1,
            FileKind::Wgsl => report.wgsl_files += 1,
        }
        loaded.push((rel, kind, text));
    }
    let rust_sources: Vec<String> = loaded
        .iter()
        .filter(|(_, kind, _)| matches!(kind, FileKind::Rust))
        .map(|(_, _, text)| text.clone())
        .collect();
    let names = reference_names(workspace_root, &rust_sources);
    for (rel, kind, text) in &loaded {
        report.hits.extend(scan_file(rel, *kind, text, &names));
    }
    report.hits.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    Ok(report)
}

/// Runs the full scan over a workspace tree, with the file-count floor
/// that tells a broken walk apart from a legitimately tiny tree.
/// Production and the CLI always go through this.
pub fn scan_workspace(workspace_root: &Path) -> Result<Report> {
    let files = collect_scan_files(workspace_root)?;
    if files.len() < MIN_FILES_SCANNED {
        bail!(
            "comment-voice scan found only {} .rs/.md/.wgsl files under {:?} (floor: \
             {MIN_FILES_SCANNED}) -- the walk is broken, not the tree; this must FAIL, not \
             report a clean pass over nothing",
            files.len(),
            workspace_root
        );
    }
    scan_paths(&files, workspace_root)
}

/// Applies the allowlist to a scanned report in place, tagging each hit and
/// collecting stale (zero-hit) entries.
fn apply_allowlist(report: &mut Report, entries: &[AllowEntry]) {
    let mut used = vec![false; entries.len()];
    for hit in &mut report.hits {
        if let Some(pos) = entries
            .iter()
            .position(|e| {
                e.file == hit.file
                    && match (e.text.as_deref(), e.line) {
                        // `text` is checked first and on its own: it is the
                        // drift-proof key, so an entry carrying one is not
                        // also bound to a line that will move.
                        (Some(text), _) => hit.snippet.contains(text),
                        (None, Some(line)) => line == hit.line,
                        (None, None) => true,
                    }
            })
        {
            used[pos] = true;
            hit.allowed = Some(AllowedBy {
                owner: entries[pos].owner.clone(),
                reason: entries[pos].reason.clone(),
            });
        }
    }
    report.stale_allow_entries = entries
        .iter()
        .zip(used)
        .filter(|(_, used)| !used)
        .map(|(e, _)| match (&e.text, e.line) {
            (Some(t), _) => format!("{} text={t:?}", e.file),
            (None, Some(l)) => format!("{}:{l}", e.file),
            (None, None) => e.file.clone(),
        })
        .collect();
}

#[must_use]
pub fn format_report(report: &Report) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "== Lodestone comment-voice guard ==");
    let _ = writeln!(
        out,
        "files scanned: {} ({} .rs, {} .md, {} .wgsl)",
        report.files_scanned, report.rs_files, report.md_files, report.wgsl_files
    );
    let _ = writeln!(out, "hits found: {}", report.hits.len());
    let _ = writeln!(out);
    for hit in &report.hits {
        let verdict = match &hit.allowed {
            Some(a) => format!("ALLOWED (owner={}, reason={})", a.owner, a.reason),
            None => "VIOLATION".to_owned(),
        };
        let _ = writeln!(
            out,
            "  {}:{} [{}] {verdict} -- {:?}",
            hit.file,
            hit.line,
            hit.kind.label(),
            hit.snippet
        );
    }
    let _ = writeln!(out);
    if !report.stale_allow_entries.is_empty() {
        let _ = writeln!(
            out,
            "NOTE: {} allowlist entr{} matched zero hits this run (safe to remove):",
            report.stale_allow_entries.len(),
            if report.stale_allow_entries.len() == 1 { "y" } else { "ies" }
        );
        for entry in &report.stale_allow_entries {
            let _ = writeln!(out, "  - {entry}");
        }
        let _ = writeln!(out);
    }
    let violations = report.violations();
    if violations.is_empty() {
        let _ = writeln!(
            out,
            "RESULT: PASS -- every hit is either absent or covered by {DEFAULT_ALLOWLIST}."
        );
    } else {
        let _ = writeln!(
            out,
            "RESULT: FAIL -- {} unallowed comment-voice hit(s):",
            violations.len()
        );
        for hit in &violations {
            let _ = writeln!(out, "  - {}:{} [{}]", hit.file, hit.line, hit.kind.label());
        }
    }
    out
}

/// The gate: scan, load the allowlist, print the full census (so a passing
/// run still shows what it looked at -- per the "no findings must never
/// share a value with 'could not look'" rule `check-ptr-const` follows),
/// and fail loudly on any unallowed hit.
pub fn run_check_comment_voice(workspace_root: &Path, allowlist_path: &Path) -> Result<()> {
    let mut report = scan_workspace(workspace_root)?;
    let entries = load_allowlist(workspace_root, allowlist_path)?;
    apply_allowlist(&mut report, &entries);
    print!("{}", format_report(&report));
    let violations = report.violations();
    if !violations.is_empty() {
        bail!(
            "RESULT: FAIL -- {} comment-voice hit(s) are not covered by {}; see the census \
             above for file:line. Fix: replace the reference/phrase with the substance it \
             pointed at, or add a reviewed [[allow]] entry with an owner and a reason.",
            violations.len(),
            allowlist_path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Workspace {
        dir: tempfile::TempDir,
    }

    impl Workspace {
        fn new() -> Result<Self> {
            Ok(Self {
                dir: tempfile::tempdir()?,
            })
        }

        fn write(&self, relative: &str, contents: &str) -> Result<()> {
            let path = self.dir.path().join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, contents)?;
            Ok(())
        }

        fn root(&self) -> &Path {
            self.dir.path()
        }
    }

    fn scan_fixture(root: &Path) -> Result<Report> {
        let files = collect_scan_files(root)?;
        scan_paths(&files, root)
    }

    const GOAL_TABLE: &str = "name_a\tFooBarGoal\nname_b\tOwnedGoal\nname_c\tOuter.NestedGoal(ARG)\nname_d\t-\n";

    #[test]
    fn a_vanilla_goal_name_in_a_comment_or_doc_fails_and_a_clean_comment_does_not() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(GOAL_NAMES_PATH, GOAL_TABLE)?;
        ws.write(
            "crates/fixture/src/bad.rs",
            "/// Mirrors `FooBarGoal` closely.\n// and Outer's NestedGoal too\npub fn f() {}\n",
        )?;
        ws.write("docs/bad.md", "The `FooBarGoal` row.\n")?;
        // Control: the same words, no goal name.
        ws.write(
            "crates/fixture/src/clean.rs",
            "/// Strolls and avoids water.\npub fn g() { let _ = \"FooBarGoal\"; }\n",
        )?;
        let report = scan_fixture(ws.root())?;
        let hits: Vec<_> = report
            .hits
            .iter()
            .filter(|h| h.kind == PatternKind::VanillaGoalName)
            .map(|h| (h.file.as_str(), h.line))
            .collect();
        assert_eq!(
            hits,
            vec![("crates/fixture/src/bad.rs", 1), ("crates/fixture/src/bad.rs", 2), ("docs/bad.md", 1)],
            "{:#?}",
            report.hits
        );
        Ok(())
    }

    #[test]
    fn a_goal_name_this_workspace_defines_itself_is_not_flagged() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(GOAL_NAMES_PATH, GOAL_TABLE)?;
        ws.write(
            "crates/fixture/src/lib.rs",
            "pub struct OwnedGoal;\n/// Drives an `OwnedGoal` and a `FooBarGoal`.\npub fn f() {}\n",
        )?;
        let report = scan_fixture(ws.root())?;
        let hits: Vec<_> = report.hits.iter().filter(|h| h.kind == PatternKind::VanillaGoalName).collect();
        assert_eq!(hits.len(), 1, "only the unowned name is flagged: {hits:#?}");
        assert!(hits[0].snippet.contains("FooBarGoal"));
        Ok(())
    }

    #[test]
    fn a_reference_class_name_fails_in_any_scanned_tree_and_not_when_the_workspace_owns_it() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(CLASS_NAMES_PATH, "AbstractFoo\nOwnType\n")?;
        // Flagged. Whole-identifier, case-sensitive: `AbstractFooBar` and
        // `abstractfoo` are controls that must not match.
        ws.write(
            "crates/lodestone-entity/src/bad.rs",
            "/// Mirrors `AbstractFoo` closely.\n// AbstractFooBar and abstractfoo are fine\npub fn f() {}\n",
        )?;
        // Any other tree is held to the same rule.
        ws.write("crates/other/src/elsewhere.rs", "/// Mirrors `AbstractFoo` closely.\npub fn g() {}\n")?;
        // The name is one our own code defines.
        ws.write(
            "crates/lodestone-entity/src/own.rs",
            "pub struct OwnType;\n/// Wraps an `OwnType`.\npub fn h() {}\n",
        )?;
        // A string literal is code, not a comment.
        ws.write(
            "crates/lodestone-entity/src/literal.rs",
            "pub fn i() -> &'static str { \"AbstractFoo\" }\n",
        )?;
        let report = scan_fixture(ws.root())?;
        let hits: Vec<_> = report
            .hits
            .iter()
            .filter(|h| h.kind == PatternKind::VanillaClassName)
            .map(|h| (h.file.as_str(), h.line))
            .collect();
        assert_eq!(
            hits,
            vec![("crates/lodestone-entity/src/bad.rs", 1), ("crates/other/src/elsewhere.rs", 1)],
            "{:#?}",
            report.hits
        );
        Ok(())
    }

    /// Controls for the method/field detector: each `assert` names the case it
    /// exercises, and the cited forms must fail while the look-alikes pass.
    #[test]
    fn a_reference_member_name_fails_only_when_cited_and_not_when_the_workspace_spells_it() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(MEMBER_NAMES_PATH, "setBlockState\nlastHurtByMob\ndataKey\ntick\n")?;
        ws.write(CLASS_NAMES_PATH, "MoonPhase\n")?;
        ws.write(
            "crates/fixture/src/lib.rs",
            concat!(
                "/// Calls `setBlockState(pos, state)` here.\n",
                "// Reads Entity.lastHurtByMob directly.\n",
                "/// Mirrors Level::setBlockState and Level#setBlockState.\n",
                "// A bare camelCase setBlockState is still a citation.\n",
                "// The snake_case set_block_state( is ours.\n",
                "// An unlisted otherName() call is fine.\n",
                "// The workspace spells dataKey() in a string below.\n",
                "// A bare lowercase tick is plain English.\n",
                "// MoonPhase.PHASE_LENGTH is a reference constant; MoonPhase.new is ours.\n",
                "pub fn f() -> &'static str { \"dataKey\" }\n",
                "pub struct MoonPhase;\n",
            ),
        )?;
        let report = scan_fixture(ws.root())?;
        let hits: Vec<_> = report
            .hits
            .iter()
            .filter(|h| h.kind == PatternKind::VanillaMemberName)
            .map(|h| h.line)
            .collect();
        assert_eq!(hits, vec![1, 2, 3, 3, 4, 9], "{:#?}", report.hits);
        Ok(())
    }

    #[test]
    fn own_code_identifiers_ignores_comments_and_literals() {
        let own = own_code_identifiers(&["// Hidden\nstruct Shown;\nconst S: &str = \"Quoted\";\n".to_string()]);
        assert!(own.contains("Shown"));
        assert!(!own.contains("Hidden"), "{own:?}");
        assert!(!own.contains("Quoted"), "{own:?}");
    }

    #[test]
    fn nested_worktrees_are_not_scanned() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write("src/lib.rs", "// current checkout\n")?;
        ws.write(".worktrees/other/src/lib.rs", "// see #295\n")?;
        let report = scan_fixture(ws.root())?;
        assert_eq!(report.files_scanned, 1);
        assert!(report.hits.is_empty(), "{:#?}", report.hits);
        Ok(())
    }

    #[test]
    fn issue_reference_in_a_line_comment_is_found() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/lib.rs",
            "// see #295's Job 2 for why this is shaped like this\npub fn f() {}\n",
        )?;
        let report = scan_fixture(ws.root())?;
        let hits: Vec<_> = report
            .hits
            .iter()
            .filter(|h| h.kind == PatternKind::IssueReference)
            .collect();
        assert_eq!(hits.len(), 1, "{:#?}", report.hits);
        assert_eq!(hits[0].line, 1);
        Ok(())
    }

    /// The exact false-positive class the issue that added this scanner
    /// measured at 431 hits with a naive substring search: "this process"
    /// and "this property" must never match "this PR".
    #[test]
    fn this_pr_does_not_match_this_process_or_this_property() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/lib.rs",
            "// this process handles it, and this property controls it\npub fn f() {}\n",
        )?;
        let report = scan_fixture(ws.root())?;
        let pr_hits: Vec<_> = report
            .hits
            .iter()
            .filter(|h| h.kind == PatternKind::ThisPr)
            .collect();
        assert!(
            pr_hits.is_empty(),
            "must not match 'this process'/'this property': {:#?}",
            report.hits
        );
        Ok(())
    }

    #[test]
    fn this_pr_matches_a_real_occurrence() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/lib.rs",
            "// this PR renamed the field\npub fn f() {}\n",
        )?;
        let report = scan_fixture(ws.root())?;
        let pr_hits: Vec<_> = report
            .hits
            .iter()
            .filter(|h| h.kind == PatternKind::ThisPr)
            .collect();
        assert_eq!(pr_hits.len(), 1, "{:#?}", report.hits);
        Ok(())
    }

    #[test]
    fn before_this_change_suppresses_the_nested_this_change_hit() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/lib.rs",
            "// before this change the field was one byte\npub fn f() {}\n",
        )?;
        let report = scan_fixture(ws.root())?;
        assert_eq!(report.hits.len(), 1, "{:#?}", report.hits);
        assert_eq!(report.hits[0].kind, PatternKind::BeforeThisChange);
        Ok(())
    }

    #[test]
    fn derive_attribute_hex_colour_and_url_fragment_are_not_issue_references() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/lib.rs",
            "// #[derive(Clone)] is not an issue; #1a2b3c is a colour; see guide.md#123-notes\n\
             #[derive(Clone)]\npub struct S;\n",
        )?;
        let report = scan_fixture(ws.root())?;
        let refs: Vec<_> = report
            .hits
            .iter()
            .filter(|h| h.kind == PatternKind::IssueReference)
            .collect();
        assert!(refs.is_empty(), "expected zero false positives: {:#?}", report.hits);
        Ok(())
    }

    #[test]
    fn a_real_issue_reference_next_to_a_paren_is_found() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/lib.rs",
            "// fixes a race (#553) in the scheduler\npub fn f() {}\n",
        )?;
        let report = scan_fixture(ws.root())?;
        let refs: Vec<_> = report
            .hits
            .iter()
            .filter(|h| h.kind == PatternKind::IssueReference)
            .collect();
        assert_eq!(refs.len(), 1, "{:#?}", report.hits);
        Ok(())
    }

    #[test]
    fn a_string_literal_containing_hash_digits_and_slash_slash_is_not_scanned() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/lib.rs",
            "pub const URL: &str = \"http://example.com/page#123\";\n",
        )?;
        let report = scan_fixture(ws.root())?;
        assert!(report.hits.is_empty(), "{:#?}", report.hits);
        Ok(())
    }

    #[test]
    fn a_lifetime_is_not_mistaken_for_a_char_literal() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/lib.rs",
            "// holds a &'static str, see #42 for why\npub fn f(_: &'static str) {}\n",
        )?;
        let report = scan_fixture(ws.root())?;
        let refs: Vec<_> = report
            .hits
            .iter()
            .filter(|h| h.kind == PatternKind::IssueReference)
            .collect();
        assert_eq!(refs.len(), 1, "the comment's own #42 must still be found: {:#?}", report.hits);
        Ok(())
    }

    #[test]
    fn a_block_comment_issue_reference_is_found() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/lib.rs",
            "/* tracked in #77 */\npub fn f() {}\n",
        )?;
        let report = scan_fixture(ws.root())?;
        let refs: Vec<_> = report
            .hits
            .iter()
            .filter(|h| h.kind == PatternKind::IssueReference)
            .collect();
        assert_eq!(refs.len(), 1, "{:#?}", report.hits);
        Ok(())
    }

    #[test]
    fn markdown_fenced_code_is_masked_but_prose_is_scanned() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "docs/fixture.md",
            "prose says this change fixed it\n\n```\nlet x = 1; // this change #999\n```\n",
        )?;
        let report = scan_fixture(ws.root())?;
        assert_eq!(
            report.hits.len(),
            1,
            "only the prose line, not the fenced code, must be scanned: {:#?}",
            report.hits
        );
        assert_eq!(report.hits[0].line, 1);
        Ok(())
    }

    #[test]
    fn wgsl_line_comment_issue_reference_is_found() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/shaders/x.wgsl",
            "// gamma-correct per #612\nfn main() {}\n",
        )?;
        let report = scan_fixture(ws.root())?;
        let refs: Vec<_> = report
            .hits
            .iter()
            .filter(|h| h.kind == PatternKind::IssueReference)
            .collect();
        assert_eq!(refs.len(), 1, "{:#?}", report.hits);
        Ok(())
    }

    #[test]
    fn allowed_hit_is_tagged_and_does_not_count_as_a_violation() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write("crates/fixture/src/lib.rs", "// see #10\npub fn f() {}\n")?;
        let mut report = scan_fixture(ws.root())?;
        let entries = parse_allowlist(
            "[[allow]]\nfile = \"crates/fixture/src/lib.rs\"\nowner = \"someone\"\nreason = \"pending sweep\"\n",
        )?;
        apply_allowlist(&mut report, &entries);
        assert!(report.violations().is_empty(), "{:#?}", report.violations());
        assert!(report.stale_allow_entries.is_empty());
        Ok(())
    }

    #[test]
    fn a_line_scoped_allow_entry_does_not_cover_a_different_line() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/lib.rs",
            "// see #10\npub fn f() {}\n// see #11\npub fn g() {}\n",
        )?;
        let mut report = scan_fixture(ws.root())?;
        let entries = parse_allowlist(
            "[[allow]]\nfile = \"crates/fixture/src/lib.rs\"\nline = 1\nowner = \"someone\"\nreason = \"pending sweep\"\n",
        )?;
        apply_allowlist(&mut report, &entries);
        assert_eq!(report.violations().len(), 1, "{:#?}", report.violations());
        assert_eq!(report.violations()[0].line, 3);
        Ok(())
    }

    #[test]
    fn allowlist_entry_missing_a_reason_is_rejected() {
        let err = parse_allowlist("[[allow]]\nfile = \"x.rs\"\nowner = \"someone\"\n")
            .expect_err("missing reason must be rejected");
        assert!(err.to_string().contains("reason"), "{err}");
    }

    #[test]
    fn duplicate_allowlist_entries_are_rejected() {
        let err = parse_allowlist(
            "[[allow]]\nfile = \"x.rs\"\nowner = \"a\"\nreason = \"r\"\n\
             [[allow]]\nfile = \"x.rs\"\nowner = \"b\"\nreason = \"r2\"\n",
        )
        .expect_err("duplicate file (both whole-file) must be rejected");
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    /// The floor exists so a broken walk fails loudly instead of reporting
    /// a clean pass over zero files -- mirrors `check-ptr-const`'s own
    /// test of the same trap.
    #[test]
    fn a_workspace_below_the_file_floor_is_a_hard_failure() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write("crates/fixture/src/lib.rs", "// see #1\n")?;
        let err = scan_workspace(ws.root()).expect_err("one file must be under the floor");
        assert!(err.to_string().contains("floor"), "{err}");
        Ok(())
    }
}
