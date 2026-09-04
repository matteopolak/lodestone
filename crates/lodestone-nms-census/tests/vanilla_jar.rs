//! Executable-use gate against the pinned local Paper 26.2 server jar.
//!
//! This stays ignored because the jar is a user-supplied, untracked measurement
//! input. When deliberately enabled, the exact baseline detects a parser that
//! loses instruction alignment while still returning a superficially plausible
//! census.

use std::path::PathBuf;
use std::process::Command;

use lodestone_nms_census::{Census, ScanOptions};

/// The paperclip-materialised server jar for Paper 26.2 build 121.
const PAPER_26_2_BUILD_121_SHA256: &str =
    "4dcefacecd1c67b4c277ae1f290c7273cc31db590f72293a6880526beeebc05a";

#[test]
#[ignore = "requires the local Paper 26.2 server jar"]
fn the_pinned_paper_jar_reproduces_the_executable_member_baseline() {
    let jar = std::env::var_os("LODESTONE_NMS_CENSUS_JAR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(".cache/paper/26.2/work/versions/26.2/paper-26.2.jar")
        });
    assert!(
        jar.is_file(),
        "the ignored real-jar gate needs {}; run the documented paperclip materialisation first or set LODESTONE_NMS_CENSUS_JAR",
        jar.display()
    );
    assert_eq!(
        sha256(&jar),
        PAPER_26_2_BUILD_121_SHA256,
        "the baseline is only valid for the documented Paper 26.2 build 121 materialisation"
    );

    let census = Census::scan_jar(&jar, &ScanOptions::default()).expect("pinned Paper jar scans");
    assert_eq!(census.classes_scanned, 10_353, "recorded class baseline");
    assert_eq!(census.parse_failure_count(), 0, "every class parses");
    assert_eq!(
        census.external_classes().len(),
        1_395,
        "distinct static-use class baseline"
    );
    assert_eq!(
        census.external_members().len(),
        7_179,
        "distinct static member-operation baseline"
    );
    assert_eq!(
        census.external_members().iter().map(|(_, stat)| stat.external).sum::<u64>(),
        16_853,
        "external static instruction-site baseline"
    );
    assert_eq!(
        census.external_symbolic_members().len(),
        6_991,
        "symbolic constant-pool baseline stays independently visible"
    );
}

fn sha256(path: &PathBuf) -> String {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .expect("the ignored macOS gate needs shasum to validate its input provenance");
    assert!(
        output.status.success(),
        "shasum failed while validating {}",
        path.display()
    );
    String::from_utf8(output.stdout)
        .expect("sha256 output is UTF-8")
        .split_whitespace()
        .next()
        .expect("sha256 output contains a digest")
        .to_owned()
}
