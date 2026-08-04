//! Regenerates `fixtures/composed_seed42.txt` from a real vanilla 26.2 JVM
//! oracle run.
//!
//! ```text
//! cargo run -p lodestone-worldgen-parity --bin regen
//! ```
//!
//! Runs `scripts/worldgen-oracle/run.sh ComposedChunkOracle`, which boots an
//! ephemeral `eclipse-temurin:25-jdk` Docker container (no local JDK needed —
//! see `CLAUDE.md`), captures its stdout, run-length-encodes it, and
//! overwrites the committed fixture. Byte-identical output on an unchanged
//! game version/seed/coordinate set is the whole point: if this produces a
//! diff, either the fixture was stale or something about the oracle's
//! environment changed — read the diff before committing it.
//!
//! `--raw-file <path>` skips Docker and reads a previously-captured raw dump
//! instead (`ComposedChunkOracle`'s stdout, unmodified) — used by this
//! binary's own test coverage, and useful if you already have a dump handy
//! and want to avoid a second container run.
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_path() -> PathBuf {
    manifest_dir().join("fixtures/composed_seed42.txt")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let raw = if let Some(pos) = args.iter().position(|a| a == "--raw-file") {
        let path = args.get(pos + 1).expect("--raw-file needs a path");
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {path}: {e}"))
    } else {
        run_oracle()
    };

    let fixtures = lodestone_worldgen_parity::parse_raw_dump(&raw);
    assert!(
        !fixtures.is_empty(),
        "oracle produced no 'meta.done' chunks — did ComposedChunkOracle actually run? \
         first 500 chars of output:\n{}",
        &raw[..raw.len().min(500)]
    );
    for f in &fixtures {
        eprintln!(
            "chunk ({}, {}): postsurface non-air {}, postcarve non-air {}, {} distinct biome quarts",
            f.chunk_x,
            f.chunk_z,
            f.postsurface.non_air_count(),
            f.postcarve.non_air_count(),
            f.biome_quarts
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
        );
    }

    let compact = lodestone_worldgen_parity::encode_compact(&fixtures);
    let path = fixture_path();
    std::fs::write(&path, &compact).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
    eprintln!(
        "wrote {} ({} bytes, {} chunks) — review with `git diff -- {}` before committing",
        path.display(),
        compact.len(),
        fixtures.len(),
        path.display(),
    );
}

fn run_oracle() -> String {
    let script = manifest_dir().join("../../scripts/worldgen-oracle/run.sh");
    assert!(script.exists(), "missing {}", script.display());
    eprintln!("running scripts/worldgen-oracle/run.sh ComposedChunkOracle (docker run --rm eclipse-temurin:25-jdk)...");
    let output = Command::new("bash")
        .arg(&script)
        .arg("ComposedChunkOracle")
        .output()
        .unwrap_or_else(|e| panic!("running {}: {e}", script.display()));
    if !output.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        panic!("ComposedChunkOracle exited with {}", output.status);
    }
    String::from_utf8(output.stdout).expect("oracle stdout is UTF-8")
}
