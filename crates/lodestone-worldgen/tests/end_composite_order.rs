use std::collections::HashMap;


const FIXTURE: &str = include_str!("support/end_composite_order_jvm.txt");

#[derive(Debug)]
struct Case {
    id: String,
    seed: i64,
    origin: String,
    expected_order: String,
    wrong_order: String,
    expected_digest: String,
    wrong_digest: String,
    probes: HashMap<String, (String, String)>,
}

fn fixture_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    let mut current = None;
    for line in FIXTURE.lines().filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let fields: Vec<_> = line.split_whitespace().collect();
        match fields[0] {
            "format" => assert_eq!(fields[1], "end-composite-order-v1"),
            "case" => {
                if let Some(case) = current.replace(Case {
                    id: fields[1].to_owned(),
                    seed: fields[2].trim_start_matches("seed=").parse().expect("case seed"),
                    origin: fields[3].trim_start_matches("origin=").to_owned(),
                    expected_order: fields[4].trim_start_matches("expected=").to_owned(),
                    wrong_order: fields[5].trim_start_matches("wrong=").to_owned(),
                    expected_digest: String::new(),
                    wrong_digest: String::new(),
                    probes: HashMap::new(),
                }) {
                    cases.push(case);
                }
            }
            "expected_digest" => current.as_mut().expect("digest before case").expected_digest = fields[1].to_owned(),
            "wrong_digest" => current.as_mut().expect("digest before case").wrong_digest = fields[1].to_owned(),
            "probe" => {
                let case = current.as_mut().expect("probe before case");
                let expected = fields[2].trim_start_matches("expected=").to_owned();
                let wrong = fields[3].trim_start_matches("wrong=").to_owned();
                case.probes.insert(fields[1].to_owned(), (expected, wrong));
            }
            other => panic!("unknown fixture record {other}"),
        }
    }
    if let Some(case) = current {
        cases.push(case);
    }
    cases
}

fn assert_fixture(case: &Case, digest: &str) {
    assert_eq!(digest, case.expected_digest, "{} composite order", case.id);
}

#[test]
fn external_capture_has_discriminating_composite_orders() {
    let cases = fixture_cases();
    assert_eq!(cases.len(), 2);
    assert_eq!(cases[0].expected_order, "outer_island,structure");
    assert_eq!(cases[0].wrong_order, "structure,outer_island");
    assert_eq!(cases[0].seed, 918_273);
    assert_eq!(cases[0].origin, "0,50,0");
    assert_eq!(cases[1].expected_order, "chorus,platform");
    assert_eq!(cases[1].wrong_order, "platform,chorus");
    assert_eq!(cases[1].seed, 12_345);
    assert_eq!(cases[1].origin, "0,65,0");
    for case in &cases {
        assert_eq!(case.expected_digest.len(), 64, "{} expected digest", case.id);
        assert_eq!(case.wrong_digest.len(), 64, "{} wrong digest", case.id);
        assert_ne!(case.expected_digest, case.wrong_digest, "{} order is not observable", case.id);
        assert!(!case.probes.is_empty(), "{} has no probe control", case.id);
        assert!(case.probes.values().any(|(expected, wrong)| expected != wrong), "{} probes are not discriminating", case.id);
    }
    let first = &cases[0];
    assert_eq!(first.probes["0,50,0"].0, "minecraft:purpur_block");
    assert_eq!(first.probes["0,50,0"].1, "minecraft:end_stone");
    let second = &cases[1];
    assert_eq!(second.probes["0,65,0"].0, "minecraft:air");
    assert!(second.probes["0,65,0"].1.starts_with("minecraft:chorus_plant"));

}

#[test]
#[should_panic(expected = "composite order")]
fn deliberately_wrong_order_negative_control_fails() {
    let case = fixture_cases().into_iter().next().expect("order fixture case");
    assert_fixture(&case, &case.wrong_digest);
}
