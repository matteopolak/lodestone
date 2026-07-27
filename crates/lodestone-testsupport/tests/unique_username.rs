use std::collections::HashSet;

#[test]
fn usernames_are_unique_short_and_keep_counter_before_truncation() {
    const N: usize = 128;

    let mut handles = Vec::new();
    for _ in 0..N {
        handles.push(std::thread::spawn(lodestone_testsupport::unique_username));
    }

    let names: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let unique: HashSet<&str> = names.iter().map(String::as_str).collect();

    assert_eq!(unique.len(), N, "duplicate usernames: {names:?}");
    assert!(
        names.iter().all(|n| n.len() <= 16),
        "all names must fit vanilla's 16-char cap: {names:?}"
    );
    for name in names {
        assert!(
            name.starts_with('E'),
            "counter prefix must come first: {name}"
        );
        let (counter, _) = name
            .split_once('_')
            .expect("name includes counter/stamp separator");
        assert!(
            counter.len() >= 2,
            "counter must not be truncated away: {name}"
        );
        assert!(
            counter[1..].chars().all(|c| c.is_ascii_alphanumeric()),
            "counter is base36 and first in the name: {name}"
        );
    }
}
