use std::path::PathBuf;

#[test]
fn check_counter_panics_when_nothing_was_checked() {
    let result = std::panic::catch_unwind(|| {
        let checked = lodestone_testsupport::CheckCounter::new("entity comparisons");
        checked.assert_at_least(1);
    });

    assert!(result.is_err(), "zero checks must be loud, not vacuous");
}

#[test]
fn check_counter_records_explicit_floor() {
    let mut checked = lodestone_testsupport::CheckCounter::new("slot comparisons");
    checked.mark();
    checked.add(2);

    assert_eq!(checked.count(), 3);
    checked.assert_at_least(3);
}

#[test]
fn fixture_selection_requires_exact_name_instead_of_read_dir_order() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("fixture-selection-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("minecraft-1.12.2-client.jar"), b"old").unwrap();
    std::fs::write(root.join("minecraft-1.21.6-client.jar"), b"new").unwrap();

    let selected = lodestone_testsupport::fixture_by_name(&root, "minecraft-1.21.6-client.jar")
        .expect("named fixture exists");
    assert_eq!(selected.file_name().unwrap(), "minecraft-1.21.6-client.jar");

    let missing = lodestone_testsupport::fixture_by_name(&root, "blocks.json").unwrap_err();
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);

    std::fs::remove_dir_all(&root).unwrap();
}
