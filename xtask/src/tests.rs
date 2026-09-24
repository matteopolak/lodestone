use super::*;
    use anyhow::Result;
    use std::{collections::BTreeSet, ops::Deref, path::Path, process::Command};

    const REAL_REPORT: &str = ".cache/mc/26.2/generated/reports/packets.json";

// Keep test groups in focused, topic-named files so this module remains navigable.
include!("tests/wasm.rs");
include!("tests/docs_benchmarks.rs");
include!("tests/packet_connectedness.rs");
include!("tests/conformance_registries.rs");
include!("tests/isolation_deletability.rs");
include!("tests/workspace_fixtures.rs");
