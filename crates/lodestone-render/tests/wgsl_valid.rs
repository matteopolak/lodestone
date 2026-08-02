//! Every `.wgsl` file this crate ships must parse and validate under naga's
//! WGSL front end — with no GPU, no adapter, and no `#[ignore]`.
//!
//! # Why this exists
//!
//! A WGSL syntax or type error in this crate used to be invisible to every
//! required health check. `cargo check --workspace --all-targets` compiles the
//! Rust that *embeds* the shader, never the shader itself; the first thing that
//! actually reads the WGSL is `Device::create_shader_module`, which only runs
//! inside the `#[ignore]`d GPU gates. So a broken shader could reach `main`
//! with all three `cargo check`s green — the same shape as the doctest gap
//! recorded in `CLAUDE.md`: a category of breakage no required check can see.
//!
//! `naga` is the exact front end wgpu itself uses (reached here as
//! `wgpu::naga`, which wgpu re-exports from `wgpu-core` on native targets — no
//! extra dependency), so this is not an approximation of the real check. It is
//! the real check, minus the adapter.
//!
//! # Scope, honestly
//!
//! This proves each shader *parses and type-checks in isolation*. It cannot
//! prove a pipeline will build: bind-group indices matching the Rust-side
//! layouts, vertex attribute locations matching the buffer layout, and entry
//! point names matching `VertexState::entry_point` are all cross-module facts
//! that only pipeline creation checks. The GPU gates remain the end-to-end
//! instrument. What this catches is the whole class of "the WGSL itself is
//! malformed", which is what actually goes wrong when a shader is edited.

use std::path::{Path, PathBuf};

/// Floor, not an exact count: a new shader must not have to touch this test,
/// but an empty or misdirected `shader_dir()` must not pass either. Without a
/// floor this is the *precondition* species of vacuous test — a wrong path
/// yields zero files and a green run.
const MIN_SHADERS: usize = 11;

fn shader_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/shaders")
}

fn wgsl_files() -> Vec<PathBuf> {
    let dir = shader_dir();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "wgsl"))
        .collect();
    files.sort();
    files
}

/// Parse + validate `src`, returning the naga diagnostic on failure.
fn check(name: &str, src: &str) -> Result<(), String> {
    let module = wgpu::naga::front::wgsl::parse_str(src)
        .map_err(|e| format!("{name}: WGSL parse error\n{}", e.emit_to_string(src)))?;
    // `Capabilities::all()` on purpose: this test is about malformed WGSL, not
    // about which optional capabilities a given adapter grants. Narrowing it
    // would turn "this shader wants f16" into a failure here rather than at
    // pipeline creation, where the device's real feature set is known.
    let mut validator = wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .map_err(|e| format!("{name}: WGSL validation error\n{}", e.emit_to_string(src)))?;
    Ok(())
}

#[test]
fn every_shader_file_parses_and_validates() {
    let files = wgsl_files();
    assert!(
        files.len() >= MIN_SHADERS,
        "found only {} .wgsl files under {} (expected at least {MIN_SHADERS}) — \
         if shaders moved, fix this test's path rather than lowering the floor",
        files.len(),
        shader_dir().display()
    );

    let mut failures = Vec::new();
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(path).expect("read shader");
        if let Err(msg) = check(&name, &src) {
            failures.push(msg);
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} shaders are invalid:\n\n{}",
        failures.len(),
        files.len(),
        failures.join("\n\n")
    );
}

/// Control for the parse stage. Without this, the gate above could be green
/// because `check` never rejects anything.
#[test]
fn the_parser_rejects_malformed_wgsl() {
    // A bare double quote in *code* position — the character that used to break
    // the *Rust* build when shaders lived in `r"..."` literals. WGSL has no
    // string type, so it is a plain parse error here, reported against the
    // shader at the shader's own line number.
    //
    // Measured, and worth knowing before writing a comment: a `"` inside a WGSL
    // `//` comment is **legal and inert** — the lexer skips the comment, so this
    // test does not and cannot flag it. That is the point rather than a gap. The
    // old rule existed because the quote broke the enclosing Rust literal; in a
    // .wgsl file there is no enclosing literal, so a quote in a comment is just
    // a character. Verified by putting one in `sky_disc.wgsl`'s comment: the
    // suite stayed green, while the same quote in code position failed with
    // `error: expected expression, found "\""`.
    let err = check("control", "@fragment fn fs() { let x = \"nope\"; }")
        .expect_err("a stray double quote must not parse as WGSL");
    assert!(err.contains("parse error"), "unexpected diagnostic: {err}");
}

/// Control for the validation stage: this parses cleanly and must still fail,
/// proving the second half of `check` is live and not short-circuited.
#[test]
fn the_validator_rejects_an_invalid_module() {
    // naga's WGSL front end resolves types eagerly, so an ordinary type error is
    // caught at parse time and would not reach the validator. This module is
    // well-typed but structurally invalid — a vertex entry point with no
    // `@builtin(position)` output — so only the validator can reject it.
    let src = "@vertex fn vs_main() -> @location(0) vec4<f32> { return vec4<f32>(0.0); }";
    let err = check("control", src).expect_err("a vertex stage with no position output is invalid");
    assert!(err.contains("validation error"), "unexpected diagnostic: {err}");
}

/// Guards the *reason* the shaders moved out of Rust: nothing under `src/`
/// should hold WGSL inline again. A single `"` inside an inlined shader ends
/// the Rust raw string and rustc then parses the WGSL and the English prose
/// around it as Rust, producing errors like `error: prefix 'yet' is unknown`
/// pointing at a comment. That has happened three times.
#[test]
fn no_wgsl_is_inlined_in_rust_sources() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    let mut scanned = 0usize;
    let mut stack = vec![src_dir.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_some_and(|x| x == "rs") {
                scanned += 1;
                let text = std::fs::read_to_string(&path).expect("read rust source");
                if text.contains("@vertex") || text.contains("@fragment") {
                    offenders.push(path.strip_prefix(&src_dir).unwrap().display().to_string());
                }
            }
        }
    }
    assert!(scanned > 0, "scanned no .rs files — {} is wrong", src_dir.display());
    assert!(
        offenders.is_empty(),
        "these Rust sources contain WGSL entry-point attributes; put the shader \
         in src/shaders/*.wgsl and pull it in with include_str!: {offenders:?}"
    );
}
