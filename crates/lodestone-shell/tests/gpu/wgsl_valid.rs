//! Every `.wgsl` file this crate ships must parse and validate under naga's
//! WGSL front end — with no GPU, no adapter, and no `#[ignore]`.
//!
//! Twin of `lodestone-render/tests/wgsl_valid.rs`; the full rationale is there
//! and in `docs/shaders.md`. The short version: `cargo check` compiles the Rust
//! that *embeds* a shader, never the shader, and the first thing that reads the
//! WGSL is `create_shader_module` inside an `#[ignore]`d GPU gate — so a broken
//! shader could reach `main` with every required check green. `naga` is reached
//! as `wgpu::naga` (wgpu re-exports it from `wgpu-core` on native), so this
//! needs no extra dependency and is the same front end wgpu itself runs.
//!
//! It is duplicated rather than shared because the only crate both could pull it
//! from is `lodestone-testsupport`, which today depends on `tokio` alone; giving
//! it a `wgpu` edge to save 100 lines of test is the worse trade.

use std::path::{Path, PathBuf};

/// Floor, not an exact count — see the render twin. A wrong `shader_dir()`
/// yields zero files, and without a floor that is a green run.
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

/// Parse + validate `src`, returning naga's own diagnostic on failure.
fn check(name: &str, src: &str) -> Result<(), String> {
    let module = wgpu::naga::front::wgsl::parse_str(src)
        .map_err(|e| format!("{name}: WGSL parse error\n{}", e.emit_to_string(src)))?;
    // `Capabilities::all()` on purpose: this test is about malformed WGSL, not
    // about which optional capabilities an adapter grants.
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

/// Control for the parse stage. Without it the gate above could be green
/// because `check` never rejects anything.
#[test]
fn the_parser_rejects_malformed_wgsl() {
    // A bare double quote in *code* position — the character that used to break
    // the *Rust* build when shaders lived in `r"..."` literals. WGSL has no
    // string type, so it is a plain parse error here.
    //
    // A `"` inside a WGSL `//` comment is **legal and inert** and this test does
    // not flag it; see the render twin for the measurement. That is the point,
    // not a gap — there is no enclosing Rust literal left to terminate.
    let err = check("control", "@fragment fn fs() { let x = \"nope\"; }")
        .expect_err("a stray double quote must not parse as WGSL");
    assert!(err.contains("parse error"), "unexpected diagnostic: {err}");
}

/// Control for the validation stage: parses cleanly and must still fail, which
/// proves the second half of `check` is live rather than short-circuited.
#[test]
fn the_validator_rejects_an_invalid_module() {
    let src = "@vertex fn vs_main() -> @location(0) vec4<f32> { return vec4<f32>(0.0); }";
    let err = check("control", src).expect_err("a vertex stage with no position output is invalid");
    assert!(err.contains("validation error"), "unexpected diagnostic: {err}");
}

/// Guards the *reason* the shaders moved out of Rust: nothing under `src/`
/// should hold WGSL inline again. One `"` inside an inlined shader ends the Rust
/// raw string, and rustc then parses the WGSL and the English prose around it as
/// Rust — `error: prefix 'yet' is unknown`, pointing at a comment. Three times.
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
