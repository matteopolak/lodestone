//! The removal path is the only way out for a block entity, and what it drops
//! follows the cause.

/// Every `.rs` file under `src`, with its text.
fn sources() -> Vec<(std::path::PathBuf, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                out.push((path, text));
            }
        }
    }
    let mut out = Vec::new();
    walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
    out
}

/// The files outside test modules that mention `needle`.
fn mentions(needle: &str) -> Vec<String> {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files: Vec<String> = sources()
        .into_iter()
        .filter(|(path, text)| {
            let production = text.split("#[cfg(test)]\nmod ").next().unwrap_or(text);
            !path.components().any(|c| c.as_os_str() == "tests" || c.as_os_str() == "tests.rs") && production.contains(needle)
        })
        .map(|(path, _)| path.strip_prefix(&src).unwrap().display().to_string())
        .collect();
    files.sort();
    files
}

#[test]
fn only_the_removal_path_takes_an_entity_out_of_the_registry() {
    // `destroy_at` is the single remover; its callers are this module (for
    // every immediate removal), the registry's own deferred wrapper, and the
    // blast stage, which takes a crater's entities under the lock that
    // publishes it and hands the removals to this module to dispose, and the
    // brushing finish, which turns the block into its base block and pops the
    // entity's revealed item itself rather than the removal drops.
    assert_eq!(mentions(".destroy_at("), ["block_entities/removal.rs", "block_removal.rs", "server/brushing.rs", "tick.rs"]);
    // The deferred form is for reaction code that cannot spawn items.
    assert_eq!(mentions(".destroy_deferred("), ["random_tick/redstone.rs"]);
}

#[test]
fn the_plain_registry_removal_is_private() {
    let registry = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/block_entities.rs")).unwrap();
    assert!(registry.contains("\n    fn remove(&mut self, pos: BlockPos)"), "the registry's remove must not be visible outside block_entities");
    assert!(!registry.contains("pub fn remove(") && !registry.contains("pub(crate) fn remove("));
}

#[test]
fn the_scan_notices_a_second_remover() {
    // Control: the detector reports a needle that does appear in several files.
    assert!(mentions("BlockEntityRegistry").len() > 1);
    assert!(mentions("no_such_symbol_anywhere_xyzzy").is_empty());
}
