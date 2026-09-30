#[test]
fn a_cold_dimensions_site_search_warms_its_columns_in_parallel() {
    let world = ParallelProbeWorld {
        floor_top: 30,
        resident: false,
        columns_touched: Mutex::new(std::collections::HashSet::new()),
        threads_seen: Mutex::new(std::collections::HashSet::new()),
        first_columns: std::sync::Barrier::new(2),
        first_columns_seen: AtomicUsize::new(0),
    };
    let origin = BlockPos::new(0, 40, 0);
    // A Rayon caller bypasses the dispatcher's admission fallback. The jobs
    // still use the production pool, without serializing under test contention.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .expect("portal parallelism probe pool must build");
    pool.install(|| {
        let _ = create_portal(&world, Dimension::Nether, origin, Axis::X);
    });

    assert_eq!(
        world.columns_touched.lock().unwrap().len(),
        9,
        "the 33x33 footprint around a chunk-aligned-ish origin spans exactly 9 columns"
    );
    let threads = world.threads_seen.lock().unwrap().len();
    assert!(
        threads > 1,
        "a 9-column prefetch used only {threads} thread(s) — the fan-out did not engage"
    );
}
