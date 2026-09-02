//! The `eula.txt` gate: a dedicated server must not start until this file
//! says `eula=true`.
//!
//! ## What it is
//!
//! Vanilla's dedicated server refuses to start at all until the operator has
//! opened `eula.txt` and flipped `eula=false` to `eula=true` by hand — see
//! `Eula.java` in this repo's own pinned 26.2 decompile
//! (`.cache/mc/26.2/src/net/minecraft/server/Eula.java`), which this module's
//! [`Gate`] mirrors mechanically: same file, same single boolean key, same
//! "absent or false refuses" rule.
//!
//! ## The wording, and why it says what it says
//!
//! **What text the file prints is a legal/policy question, not a code one —
//! [`NOTICE`] is the single constant that answers it, decided by the
//! repository owner, not invented by this module.** The tension is real and
//! worth stating plainly: Mojang's own EULA (`https://aka.ms/MinecraftEULA`)
//! governs Mojang's server software, and Lodestone is a from-scratch
//! reimplementation, not obviously bound by an agreement written for their
//! binary — this codebase's own `docs/legal-notices.md` and its
//! non-affiliation disclaimer exist for exactly that reason. But an operator
//! running this dedicated server is still running something that speaks
//! Minecraft's protocol and joins its ecosystem, over which Mojang does
//! assert their EULA and commercial-usage guidelines regardless of whose
//! server binary is involved. [`NOTICE`] therefore points the operator at
//! Mojang's EULA as the terms governing *operating* a Minecraft-compatible
//! server, while stating plainly that Lodestone is not Mojang's software, is
//! not affiliated with Mojang, and is itself provided under its own
//! open-source license. See `docs/dedicated-server.md` for the fuller
//! picture.
//!
//! ## How it works
//!
//! [`Gate::check`] reads `eula.txt`, exactly as
//! [`crate::properties::RawProperties`] would (`eula=true`/`eula=false`,
//! case-insensitive, default `false` if the key is missing) — a full
//! `RawProperties` parse would work too, but vanilla's own file is a single
//! key, so this reads it directly rather than pulling in the ordering
//! machinery a one-key file does not need. A missing file is written fresh
//! (mirroring `Eula.saveDefaults`) and reads as "not accepted", never as an
//! error — a server directory that has never been started must refuse to
//! start, not crash.
//!
//! ## How to change it
//!
//! Any further change to the wording is a [`NOTICE`]-only edit; nothing else
//! in this module needs to change, since `check`/`agreed`/`write_template`
//! all treat it as an opaque string. Do not weaken [`Gate::check`] to accept
//! a missing file as agreement — that is the one property this gate exists
//! to enforce, and vanilla's own `Eula` does not either
//! (`SharedConstants.IS_RUNNING_IN_IDE` is a JVM-only escape hatch this port
//! has no equivalent of and should not invent one for).
//!
//! ## Configuration
//!
//! One file, `eula.txt`, in the server's root directory.
//!
//! ## Dependencies
//!
//! `std::fs` only. Native-only, like `crate::properties`.

use std::path::Path;

/// The `eula.txt` notice text — see this module's doc comment for the
/// reasoning behind it. Points the operator at Mojang's EULA as the terms
/// governing running a Minecraft-compatible server, without claiming or
/// implying that Lodestone is Mojang's software or affiliated with Mojang.
pub const NOTICE: &str =
    "By changing the setting below to TRUE you are indicating your agreement to run this \
     software. Lodestone is an independent, from-scratch reimplementation and is not Mojang's \
     software; this project is not affiliated with, endorsed by, or associated with Mojang or \
     Microsoft (see docs/legal-notices.md). Operating a Minecraft-compatible server is \
     nonetheless governed by Mojang's own End User License Agreement: \
     https://aka.ms/MinecraftEULA. Lodestone itself is provided under its own open-source \
     license (see LICENSE-MIT / LICENSE-APACHE).";

/// Whether `eula.txt` at `path` says `eula=true`.
///
/// Returns `Ok(false)` (never an error) for a missing or unreadable file —
/// same shape as vanilla's own `Eula.readFile`, whose `catch` arm logs and
/// treats the failure as "not agreed" rather than propagating.
///
/// # Errors
///
/// Only from writing a **missing** file's fresh template (`std::fs::write`);
/// reading never fails this function's return type.
pub fn check(path: &Path) -> std::io::Result<bool> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(agreed(&text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            write_template(path)?;
            Ok(false)
        }
        Err(_) => Ok(false),
    }
}

/// `Boolean.parseBoolean(properties.getProperty("eula", "false"))`: the value
/// of the `eula` key, case-insensitively `true`, defaulting to `false` when
/// the key is absent — including when the whole file is absent or empty.
fn agreed(text: &str) -> bool {
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.starts_with('!') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=').or_else(|| trimmed.split_once(':'))
        else {
            continue;
        };
        if key.trim() == "eula" {
            return value.trim().eq_ignore_ascii_case("true");
        }
    }
    false
}

fn write_template(path: &Path) -> std::io::Result<()> {
    let text = format!("#{NOTICE}\neula=false\n");
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_written_fresh_and_refuses() {
        let dir = std::env::temp_dir().join(format!(
            "lodestone-eula-test-{}-{}",
            std::process::id(),
            "a_missing_file_is_written_fresh_and_refuses"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("eula.txt");
        assert!(!path.exists());
        let accepted = check(&path).unwrap();
        assert!(!accepted, "a freshly written eula.txt must not read as accepted");
        assert!(path.exists(), "check() must write the template for a missing file");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("eula=false"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn eula_true_is_the_only_thing_that_accepts() {
        assert!(agreed("eula=true"));
        assert!(agreed("eula=TRUE"));
        assert!(agreed("#comment\r\neula=true\r\n"));
    }

    /// Control: the detector must be able to say no. Without this, a gate
    /// that always answered `true` would pass the positive test above for
    /// the wrong reason.
    #[test]
    fn absent_or_false_or_malformed_all_refuse() {
        assert!(!agreed(""));
        assert!(!agreed("eula=false"));
        assert!(!agreed("eula=nope"));
        assert!(!agreed("# just a comment, no key at all"));
        assert!(!agreed("something-else=true"));
    }
}
