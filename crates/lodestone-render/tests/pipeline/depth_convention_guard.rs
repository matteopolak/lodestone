//! Is the whole workspace still on one depth convention?
//!
//! # What it is
//!
//! This renderer projects **reversed-Z** — near maps to `1`, far to `0` (see
//! `lodestone_render::Camera::projection_matrix`). That is one decision spread
//! across roughly fifteen render pipelines, a handful of depth clears, a GUI
//! orthographic matrix and a second perspective for the first-person hand, and
//! **a partial conversion is worse than none**: a pass whose comparison says
//! "nearer wins" against an attachment cleared to the near end rejects every
//! fragment and draws nothing, with no validation error, no panic and no
//! warning.
//!
//! `cargo check` cannot see any of it — every wrong value is a well-typed
//! `wgpu` enum — and no hermetic pixel gate can either, because each builds its
//! own pass and would simply be wrong in the same direction. So this is a
//! source-text scan, which is the same shape as the `chat_opts` wiring detector
//! and `wasm-check`'s confinement rules: whenever the type system cannot express
//! a constraint, make it *checkable*.
//!
//! # What it looks for, and why each one
//!
//! | banned in `crates/*/src` | because |
//! |---|---|
//! | `LoadOp::Clear(1.0)` | `1.0` is the **near** plane under reversed-Z. A depth attachment cleared there rejects everything. Use `lodestone_render::DEPTH_CLEAR`. |
//! | `depth_compare: Some(wgpu::CompareFunction::Less…)` | `Less` and `LessEqual` are the forward-depth spellings of "nearer wins". Use `DEPTH_COMPARE_NEARER` / `DEPTH_COMPARE_NEARER_OR_EQUAL`. |
//! | `directx::perspective(` outside `camera.rs` | glam's is the **forward** projection. A second one built directly is how `hand_projection` was left projecting the opposite way to the world when the conversion landed — measured, and the reason this rule exists. |
//!
//! `CompareFunction::Equal` and `Always` are deliberately **not** banned:
//! equality is its own mirror image and `Always` has no direction, so both are
//! correct under either convention (`glint.rs`, `nametag.rs`).
//!
//! # The controls
//!
//! Two, because a scanner that reports nothing is indistinguishable from a
//! scanner that did not run:
//!
//! * [`control_the_scanner_reaches_the_files_it_claims_to`] — the walk must find
//!   a plausible number of source files, and must find the specific files these
//!   rules exist for.
//! * [`control_each_rule_matches_text_it_is_meant_to_match`] — every pattern is
//!   run against a synthetic line that must match and one that must not, so a
//!   typo'd pattern fails here rather than passing silently forever.

use std::path::{Path, PathBuf};

/// The workspace root, from this crate's manifest directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate>/ has two ancestors")
        .to_path_buf()
}

/// Every `.rs` file under `crates/*/src`, which is production code only —
/// test harnesses build their own passes and are covered by their own gates.
fn production_sources() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let crates = workspace_root().join("crates");
    let mut stack: Vec<PathBuf> = std::fs::read_dir(&crates)
        .expect("crates/ must exist")
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("src"))
        .filter(|p| p.is_dir())
        .collect();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// One banned pattern: a literal substring, the reason, and the replacement.
struct Rule {
    /// Literal substring — deliberately not a regex. A table whose entries are
    /// regexes is a table whose entries can silently fail to compile or to mean
    /// what they read as; `wasm-check.sh` shipped five such rules that errored
    /// and reported PASS for weeks.
    banned: &'static str,
    reason: &'static str,
    /// A file this rule does not apply to, if any.
    exempt: Option<&'static str>,
}

const RULES: &[Rule] = &[
    Rule {
        banned: "LoadOp::Clear(1.0)",
        reason: "1.0 is the NEAR plane under reversed-Z; a depth attachment \
                 cleared there rejects every fragment. Use \
                 `lodestone_render::DEPTH_CLEAR`.",
        exempt: None,
    },
    Rule {
        banned: "depth_compare: Some(wgpu::CompareFunction::Less)",
        reason: "`Less` is the forward-depth spelling of \"strictly nearer \
                 wins\". Use `lodestone_render::DEPTH_COMPARE_NEARER`.",
        exempt: None,
    },
    Rule {
        banned: "depth_compare: Some(wgpu::CompareFunction::LessEqual)",
        reason: "`LessEqual` is the forward-depth spelling of \"nearer or tied \
                 wins\". Use `lodestone_render::DEPTH_COMPARE_NEARER_OR_EQUAL`.",
        exempt: None,
    },
    Rule {
        banned: "directx::perspective(",
        reason: "glam's `directx::perspective` is the FORWARD projection. Every \
                 perspective in this workspace must come from \
                 `Camera::projection_matrix`, which is reversed-Z — a second one \
                 built directly is how the first-person hand pass was left \
                 projecting the opposite way to the world.",
        // `camera.rs` names it in `projection_matrix`'s doc comment and uses it
        // as the *reference* in its own test, which is the point of the test.
        exempt: Some("lodestone-render/src/camera.rs"),
    },
];

/// The rules, over the whole workspace.
#[test]
fn no_production_source_carries_a_forward_depth_spelling() {
    let files = production_sources();
    let mut findings = Vec::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let display = path.to_string_lossy().replace('\\', "/");
        for rule in RULES {
            if rule.exempt.is_some_and(|e| display.ends_with(e)) {
                continue;
            }
            for (i, line) in text.lines().enumerate() {
                if line.contains(rule.banned) {
                    findings.push(format!(
                        "  {display} (line {}) carries `{}`\n      {}",
                        i + 1,
                        rule.banned,
                        rule.reason
                    ));
                }
            }
        }
    }
    assert!(
        findings.is_empty(),
        "this workspace projects reversed-Z (near -> 1, far -> 0), and these \
         production sources still spell depth the forward way. A partial \
         conversion draws nothing and reports no error:\n{}",
        findings.join("\n")
    );
    println!("  scanned {} production source files, {} rules", files.len(), RULES.len());
}

/// Control: the walk reaches the files these rules exist for.
///
/// A scan that found nothing because it looked nowhere would pass the gate
/// above, so the population is asserted — both a floor on the count and the
/// presence of the specific files whose pipelines the rules are about.
#[test]
fn control_the_scanner_reaches_the_files_it_claims_to() {
    let files = production_sources();
    assert!(
        files.len() > 200,
        "the scan found only {} production source files, which is far below this \
         workspace's real size — the walk is not reaching `crates/*/src`",
        files.len()
    );
    for required in [
        "lodestone-render/src/model_pipeline.rs",
        "lodestone-render/src/entity_pipeline.rs",
        "lodestone-render/src/block.rs",
        "lodestone-render/src/camera.rs",
        "lodestone-shell/src/gpu/frame.rs",
        "lodestone-shell/src/gpu/sign_text.rs",
        "lodestone-shell/src/particles.rs",
    ] {
        assert!(
            files
                .iter()
                .any(|p| p.to_string_lossy().replace('\\', "/").ends_with(required)),
            "the scan did not reach {required}, which builds a depth-tested \
             pipeline — every rule above is silent for it"
        );
    }
}

/// Control: each rule's pattern matches what it is meant to match, and does not
/// match its correct replacement.
///
/// Every rule fires here, so `no_production_source_carries_a_forward_depth_spelling`
/// reporting nothing means "no violations" rather than "the patterns are typos".
/// The negative half is what stops a rule being widened into one that also bans
/// the fix — a rule matching `CompareFunction::` alone would flag every correct
/// line in the tree.
#[test]
fn control_each_rule_matches_text_it_is_meant_to_match() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "LoadOp::Clear(1.0)",
            "load: wgpu::LoadOp::Clear(1.0),",
            "load: wgpu::LoadOp::Clear(lodestone_render::DEPTH_CLEAR),",
        ),
        (
            "depth_compare: Some(wgpu::CompareFunction::Less)",
            "depth_compare: Some(wgpu::CompareFunction::Less),",
            "depth_compare: Some(DEPTH_COMPARE_NEARER),",
        ),
        (
            "depth_compare: Some(wgpu::CompareFunction::LessEqual)",
            "depth_compare: Some(wgpu::CompareFunction::LessEqual),",
            "depth_compare: Some(DEPTH_COMPARE_NEARER_OR_EQUAL),",
        ),
        (
            "directx::perspective(",
            "glam::camera::rh::proj::directx::perspective(fov, aspect, near, far)",
            "camera.projection_matrix()",
        ),
    ];
    assert_eq!(
        cases.len(),
        RULES.len(),
        "every rule must have a matching pair here, or a new rule is unproven"
    );
    let mut fired = 0usize;
    for (rule, (banned, positive, negative)) in RULES.iter().zip(cases) {
        assert_eq!(
            rule.banned, *banned,
            "the control's cases are out of step with RULES"
        );
        assert!(
            positive.contains(rule.banned),
            "rule `{}` does not match the line it exists to catch: {positive}",
            rule.banned
        );
        assert!(
            !negative.contains(rule.banned),
            "rule `{}` also matches its own correct replacement: {negative}",
            rule.banned
        );
        fired += 1;
    }
    assert_eq!(
        fired,
        RULES.len(),
        "rules that actually ran: {fired}/{}",
        RULES.len()
    );
    println!("  rules that actually ran: {fired}/{}", RULES.len());
}

/// The one thing the source scan above structurally cannot see: whether
/// `Camera::projection_matrix` is *itself* still reversed.
///
/// Every rule above bans a forward *spelling*; none of them would notice the
/// projection quietly going back to forward `[0, 1]`, at which point every one
/// of those spellings would be correct again and the whole table would be
/// backwards. So the convention is pinned here, at its single source, by
/// measurement rather than by inspection.
#[test]
fn the_projection_this_whole_convention_rests_on_is_still_reversed() {
    let camera = lodestone_render::Camera::default();
    let vp = camera.view_projection();
    let depth = |distance: f32| {
        let point = camera.position + camera.forward() * distance;
        let clip = vp * point.extend(1.0);
        clip.z / clip.w
    };
    let near = depth(camera.near);
    let far = depth(camera.far);
    assert!(
        (near - 1.0).abs() < 1e-3,
        "the near plane must map to depth 1; got {near}. If this renderer has \
         deliberately returned to a forward projection, every rule in this file \
         is inverted and the whole table must be rewritten rather than deleted."
    );
    assert!(far.abs() < 1e-3, "the far plane must map to depth 0; got {far}");
    assert_eq!(
        lodestone_render::DEPTH_CLEAR, far.round(),
        "DEPTH_CLEAR must be the far plane's depth"
    );
    assert_eq!(
        lodestone_render::DEPTH_COMPARE_NEARER,
        wgpu::CompareFunction::Greater
    );
    assert_eq!(
        lodestone_render::DEPTH_COMPARE_NEARER_OR_EQUAL,
        wgpu::CompareFunction::GreaterEqual
    );
}
