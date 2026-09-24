//! The cross-plugin-messaging dependency-direction claim, as an enforceable
//! check rather than a comment.
//!
//! `cross_plugin_message.rs` proves a message *arrives*. That is only
//! interesting if the subscriber has no compile-time dependency on the
//! publisher — otherwise it demonstrates nothing a plain function call would not.
//! So this file asserts the dependency direction as a **fact about the
//! manifests**, which is the half that would silently rot: someone adds
//! `lodestone-shop` to `[dependencies]` to reach one helper, every test still
//! passes, and the property the crates exist to demonstrate is gone.
//!
//! # What this gate is in scope for, and what it is blind to
//!
//! `CLAUDE.md`: a gate comparing two things you control cannot tell you a third
//! thing exists. This one reads two manifests and answers one question — "is
//! there a declared `lodestone-shop` dependency on a non-dev path into
//! `lodestone-shop-stats`". It is blind to:
//!
//! - **A longer transitive chain.** It checks `lodestone-shop-stats` and
//!   `lodestone-shop-api` directly. A four-crate cycle through a new crate would
//!   pass. Bounded deliberately: `cargo tree`-shaped reachability is
//!   `cargo xtask check-isolation`'s job, not a plugin's own test.
//! - **Any crate that is not one of these two.** It says nothing about the rest
//!   of `crates/plugins/`.
//! - **Whether the message actually arrives.** That is
//!   `cross_plugin_message.rs`. Neither file is sufficient alone, which is the
//!   point of having both.

use std::path::{Path, PathBuf};

/// The section a key was found in.
#[derive(Debug, PartialEq, Eq)]
enum Section {
    Dependencies,
    DevDependencies,
    BuildDependencies,
    Other,
}

/// Which sections of `manifest` declare a dependency **exactly** named `name`.
///
/// Hand-rolled rather than pulling in a `toml` dev-dependency for two files, and
/// the one subtlety is load-bearing: `lodestone-shop-api` *starts with*
/// `lodestone-shop`, so a `contains("lodestone-shop")` check reports a false
/// positive on the very dependency that is supposed to be there. Keys are
/// therefore compared for exact equality after splitting on `=`.
fn sections_declaring(manifest: &str, name: &str) -> Vec<Section> {
    let mut found = Vec::new();
    let mut section = Section::Other;
    for raw in manifest.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = match header {
                "dependencies" => Section::Dependencies,
                "dev-dependencies" => Section::DevDependencies,
                "build-dependencies" => Section::BuildDependencies,
                _ => Section::Other,
            };
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        if key.trim().trim_matches('"') != name {
            continue;
        }
        found.push(match section {
            Section::Dependencies => Section::Dependencies,
            Section::DevDependencies => Section::DevDependencies,
            Section::BuildDependencies => Section::BuildDependencies,
            Section::Other => Section::Other,
        });
    }
    found
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The headline claim: the subscriber does **not** depend on the publisher on
/// any non-dev path.
#[test]
fn the_subscriber_does_not_depend_on_the_publisher() {
    let manifest = read(&manifest_dir().join("Cargo.toml"));
    let found = sections_declaring(&manifest, "lodestone-shop");
    assert!(
        !found.contains(&Section::Dependencies),
        "lodestone-shop-stats must not have lodestone-shop in [dependencies] -- \
         the whole point of this pattern is that a subscriber needs no compile-time \
         dependency on the publisher. Found in: {found:?}"
    );
    assert!(
        !found.contains(&Section::BuildDependencies),
        "nor in [build-dependencies]. Found in: {found:?}"
    );
}

/// **The parser's control.** The assertion above is only evidence if the parser
/// can actually find a dependency when there is one — an always-empty
/// `sections_declaring` would make it vacuously true. `lodestone-shop` *is*
/// declared, under `[dev-dependencies]`, so this requires the parser to see it
/// there and nowhere else.
#[test]
fn the_parser_does_find_the_publisher_under_dev_dependencies() {
    let manifest = read(&manifest_dir().join("Cargo.toml"));
    let found = sections_declaring(&manifest, "lodestone-shop");
    assert_eq!(
        found,
        vec![Section::DevDependencies],
        "lodestone-shop must be declared exactly once, under [dev-dependencies] \
         -- if this is empty the parser is broken and the test above proves nothing"
    );
}

/// The trap this parser exists to avoid, made explicit: `lodestone-shop-api`
/// starts with `lodestone-shop`, so a substring check would conflate them and
/// report the *required* dependency as the *forbidden* one — a gate that fails
/// in the safe-looking direction.
#[test]
fn the_api_crate_is_a_real_dependency_and_is_not_confused_with_the_publisher() {
    let manifest = read(&manifest_dir().join("Cargo.toml"));
    assert_eq!(
        sections_declaring(&manifest, "lodestone-shop-api"),
        vec![Section::Dependencies],
        "the api crate is the one edge the subscriber does have"
    );
    assert!(
        manifest.contains("lodestone-shop-api"),
        "sanity: the string really is present, so the exact-match logic above is \
         what separated it from `lodestone-shop`, not its absence"
    );
}

/// One level of the transitive hole, closed: the shared `-api` crate must not
/// reach the publisher either, or the subscriber would depend on it after all.
#[test]
fn the_api_crate_does_not_depend_on_the_publisher_or_the_subscriber() {
    let api = read(
        &manifest_dir()
            .join("..")
            .join("lodestone-shop-api")
            .join("Cargo.toml"),
    );
    for name in ["lodestone-shop", "lodestone-shop-stats"] {
        assert!(
            sections_declaring(&api, name).is_empty(),
            "lodestone-shop-api must depend on neither half of the family; found {name}"
        );
    }
    // Positive control: the parser is pointed at a real manifest that does
    // declare something, so "empty" above is a finding rather than a bad path.
    assert_eq!(
        sections_declaring(&api, "lodestone-ecs"),
        vec![Section::Dependencies],
        "sanity: the api manifest was read and parsed"
    );
}
