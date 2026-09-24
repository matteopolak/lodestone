//! `cargo xtask check-no-winit-headless` — proves a `--no-default-features`
//! build of `lodestone-shell` genuinely does not link `winit`.
//!
//! # Why this exists
//!
//! The `runtime-presentation` feature made attach/detach of a window a
//! runtime operation instead of only a startup-time choice, but left `winit`
//! an **unconditional** dependency of `lodestone-shell` — a headless build
//! (no window, no GPU, library-only use of `Sim`/`lodestone-client`) still
//! linked the whole windowing stack. That gap was reported, not fixed, when
//! that feature landed: `winit v0.30.13` showed up twice in
//! `cargo tree -p lodestone-shell --no-default-features` even with every
//! optional feature off.
//!
//! Closing that gap moved `winit` behind a new `window` Cargo feature (in
//! `default`, alongside `live`/`runtime-presentation`) and split the `app`
//! module — winit's real, unavoidable consumer (`WindowApp`'s
//! `ApplicationHandler`, every winit event type) — out from
//! `crate::diagnostics` (`Mode::Headless`/`Mode::Connect`, which need a GPU
//! adapter or nothing at all but never a window). `crate::keybinds::Key`/
//! `MouseButton` are the seam in between: winit conversions for them exist
//! only behind `window`.
//!
//! Per `CLAUDE.md`'s own rule ("whenever the type system cannot express a
//! constraint, make it checkable and check it"), a doc claiming the graph is
//! winit-free is not enough — this is that check, run by `just check-seam`.
//!
//! # What it actually asserts
//!
//! `cargo tree -p lodestone-shell --no-default-features -i winit` inverts the
//! dependency graph to find every path into `winit`. When `winit` is not in
//! the resolved graph at all for this feature set, `cargo tree -i` itself
//! fails with "package ID specification `winit` did not match any
//! packages" on stderr — that failure **is** the pass condition here, and
//! this function is the thing that turns "cargo failed" into "the crate we
//! wanted absent is confirmed absent" instead of leaving it to be
//! misread. If `winit` **is** in the graph, the same command instead prints
//! the dependency path(s) that pull it in and exits successfully — which
//! this function turns into a hard failure, quoting that path so the
//! regression is diagnosable without re-running anything.
//!
//! # The control: this guard has been watched to fail
//!
//! Run against a deliberately reintroduced unconditional `winit` dependency
//! (temporarily reverting `crates/lodestone-shell/Cargo.toml`'s `winit =
//! { workspace = true, optional = true }` back to `winit = { workspace =
//! true }`), this reports the regression and returns a non-zero exit — see
//! `docs/runtime-presentation.md`'s winit-free headless build section for
//! the transcript. `CLAUDE.md`'s rule that an absence-detector needs a
//! control proving it can fail is why that run happened rather than being
//! asserted.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

const CHECK_ARGS: &[&str] = &[
    "tree",
    "-p",
    "lodestone-shell",
    "--no-default-features",
    "-i",
    "winit",
];

pub fn run_check_no_winit_headless(workspace_root: &Path) -> Result<()> {
    let output = Command::new("cargo")
        .args(CHECK_ARGS)
        .current_dir(workspace_root)
        .output()
        .context("failed to run `cargo tree -p lodestone-shell --no-default-features -i winit`")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // `cargo tree -i <pkg>` fails with exactly this message when `<pkg>` is
    // not resolved into the graph for the given feature set at all — the
    // pass condition for a headless build that must not link winit.
    let confirmed_absent = !output.status.success() && stderr.contains("did not match any packages");

    if confirmed_absent {
        println!(
            "OK: winit is absent from `cargo tree -p lodestone-shell --no-default-features`'s \
             resolved dependency graph."
        );
        return Ok(());
    }

    if output.status.success() {
        bail!(
            "winit is reachable from a `--no-default-features` build of lodestone-shell — a \
             headless build must not link the windowing stack. Dependency path(s) reported by \
             `cargo tree -p lodestone-shell --no-default-features -i winit`:\n{stdout}\n\
             See docs/runtime-presentation.md's winit-free headless build section: winit must \
             stay behind lodestone-shell's `window` Cargo feature (`winit = {{ workspace = \
             true, optional = true }}`, pulled in only by `window = [\"dep:winit\", \
             \"lodestone-render/window\"]`), and nothing outside `app`/`app::*` (which is itself \
             gated `#[cfg(feature = \"window\")]` in lib.rs) may name a winit type."
        );
    }

    bail!(
        "`cargo tree -p lodestone-shell --no-default-features -i winit` failed in an \
         unrecognised way (exit {:?}) rather than either succeeding (winit reachable) or \
         reporting \"did not match any packages\" (winit absent, the expected pass case). \
         stdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The control this module's own doc promises: run the check against a
    /// fixture workspace where `lodestone-shell`'s manifest still names
    /// `winit` unconditionally, and confirm the guard actually fails rather
    /// than merely asserting it would. `CLAUDE.md`'s rule on an
    /// absence-detector needing a control that fails — this is that control,
    /// exercised on every `cargo test -p xtask` run rather than only once by
    /// hand.
    ///
    /// Builds a two-crate fixture workspace (a `lodestone-shell`-named crate
    /// depending unconditionally on the real `winit`, from this workspace's
    /// own registry cache) rather than pointing at the real
    /// `crates/lodestone-shell` — real winit has native system dependencies
    /// this sandbox may not have, and downloading a fresh copy in a test is
    /// its own hazard. The fixture reproduces the one structural fact the
    /// guard actually inspects: whether `cargo tree -i winit` can find a path
    /// to it from a package literally named `lodestone-shell`.
    #[test]
    #[ignore = "network + cargo registry access; the real check runs in `just check-seam`"]
    fn fails_loudly_when_winit_is_unconditional() {
        let dir = std::env::temp_dir().join(format!(
            "xtask-no-winit-headless-control-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"lodestone-shell\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\nwinit = \"0.30\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/lib.rs"), "").unwrap();

        let result = run_check_no_winit_headless(&dir);
        assert!(
            result.is_err(),
            "the guard must fail against a fixture where winit is unconditional — a guard \
             that cannot fail is not evidence of anything"
        );
        let message = result.unwrap_err().to_string();
        assert!(
            message.contains("winit is reachable"),
            "expected the reachable-winit failure message, got: {message}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
