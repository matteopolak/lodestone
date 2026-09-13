use lodestone_server::heavy_scene::{
    HeavyScenario, HeavySceneSpec, HeavyServerArgs, HeavyServerHarness,
};
use lodestone_v26_2::V770ServerProtocol;

#[test]
fn canonical_plan_is_stable_and_uses_the_client_contract_names() {
    let spec = HeavySceneSpec::new(HeavyScenario::Mixed, 17, 1).unwrap();
    let first = spec.build_plan().unwrap();
    let second = spec.build_plan().unwrap();
    assert_eq!(first.scene_hash, second.scene_hash);
    assert_eq!(first.commands.setup, second.commands.setup);
    assert_eq!(first.commands.after_join, second.commands.after_join);
    assert_eq!(first.commands.mutation, second.commands.mutation);
    assert_eq!(first.json_value()["schema"], 1);
    assert_eq!(first.json_value()["spec"]["scenario"], "mixed");
    assert!(first.json_value()["commands"]["setup"].is_array());
}

#[test]
fn invalid_scenario_parameters_are_rejected_before_actions_exist() {
    assert!(HeavySceneSpec::new(HeavyScenario::Mixed, 17, 0).is_err());
    assert!(HeavySceneSpec::new(
        HeavyScenario::Mixed,
        17,
        HeavySceneSpec::MAX_SCALE + 1
    )
    .is_err());
    assert!(HeavySceneSpec::new(HeavyScenario::DenseMixed, 17, 2).is_err());
    assert!(HeavyScenario::parse_name("not-a-scenario").is_none());
}

#[test]
fn mixed_plan_contains_every_client_witness_family() {
    let plan = HeavySceneSpec::new(HeavyScenario::Mixed, 17, 1)
        .unwrap()
        .build_plan()
        .unwrap();
    for required in [
        ("world.opaque_sections_drawn", 1),
        ("world.water_sections_drawn", 1),
        ("world.translucent_sections_drawn", 1),
        ("world.entities_drawn", 1),
        ("world.block_entities_drawn", 1),
        ("world.sign_text_vertices", 6),
        ("world.particles_drawn", 1),
        ("light.relight_cells_changed", 1),
        ("light.remesh_sections_submitted", 1),
    ] {
        assert!(plan
            .witnesses
            .iter()
            .any(|w| w.column == required.0 && w.minimum == required.1));
    }
    assert!(!plan.commands.setup.is_empty());
    assert!(!plan.commands.mutation.is_empty());
}

#[test]
fn dense_mixed_plan_uses_the_bounded_high_pressure_composition() {
    let plan = HeavySceneSpec::new(HeavyScenario::DenseMixed, 17, 1)
        .unwrap()
        .build_plan()
        .unwrap();
    let count = |needle: &str| {
        plan.commands
            .setup
            .iter()
            .filter(|line| line.contains(needle))
            .count()
    };
    assert_eq!(count("summon minecraft:"), 2_048);
    assert_eq!(count("minecraft:oak_wall_sign"), 1_536);
    assert_eq!(count("minecraft:sea_lantern"), 1_024);
    assert_eq!(count("minecraft:water"), 1_024);
    assert_eq!(count("minecraft:white_stained_glass") + count("minecraft:glass_pane"), 768);
    assert_eq!(count("minecraft:repeating_command_block"), 512);
    assert_eq!(count("minecraft:chest") + count("minecraft:purple_shulker_box")
        + count("minecraft:white_banner") + count("minecraft:conduit"), 512);
    assert_eq!(plan.commands.setup.len(), 7_937);
    assert_eq!(plan.commands.after_join, ["tp @a 0 220 0 0 90"]);
    assert!(plan
        .commands
        .setup
        .iter()
        .all(|line| line.len() <= HeavySceneSpec::MAX_COMMAND_BYTES));
}

#[test]
fn entity_actions_have_unique_positions_and_commands_are_bounded() {
    let plan = HeavySceneSpec::new(HeavyScenario::Entity, 19, 2)
        .unwrap()
        .build_plan()
        .unwrap();
    let summons: Vec<_> = plan
        .commands
        .setup
        .iter()
        .filter(|line| line.starts_with("summon "))
        .collect();
    assert_eq!(summons.len(), 2048);
    assert_eq!(summons.iter().collect::<std::collections::BTreeSet<_>>().len(), summons.len());
    assert!(plan
        .commands
        .setup
        .iter()
        .all(|line| line.len() <= HeavySceneSpec::MAX_COMMAND_BYTES));
}

#[test]
fn emit_arguments_require_exact_scene_inputs() {
    let args = HeavyServerArgs::parse_from([
        "heavy-scene-server",
        "--emit-scene",
        "-",
        "--scenario",
        "mixed",
        "--seed",
        "17",
        "--scale",
        "1",
    ])
    .unwrap();
    assert_eq!(args.emit_scene.as_deref(), Some(std::path::Path::new("-")));
    assert_eq!(args.spec.scenario, HeavyScenario::Mixed);
    assert!(HeavyServerArgs::parse_from(["heavy-scene-server", "--emit-scene", "-"]).is_err());
}

#[test]
fn emitted_json_is_client_plan_compatible_and_does_not_start_a_server() {
    let spec = HeavySceneSpec::new(HeavyScenario::Sign, 17, 1).unwrap();
    let json = spec.build_plan().unwrap().json_string();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["schema"], 1);
    assert_eq!(value["spec"]["seed"], 17);
    assert!(value["commands"]["setup"].as_array().unwrap().len() >= 24);
    assert_eq!(value["scene_hash"].as_str().unwrap().len(), 64);
}

#[tokio::test]
async fn server_harness_receives_the_full_join_view_and_batch_markers() {
    let args = HeavyServerArgs::for_test(HeavyScenario::Palette);
    let plan = args.spec.build_plan().unwrap();
    let record = HeavyServerHarness::run(args, plan, V770ServerProtocol)
        .await
        .unwrap();
    assert_eq!(record.consumed.join_columns, 9);
    assert!(record.consumed.chunk_payload_bytes > 0);
    assert!(record.consumed.chunk_batches > 0);
}

#[tokio::test]
async fn server_harness_rejects_a_removed_palette_producer() {
    let args = HeavyServerArgs::for_test(HeavyScenario::Palette);
    let mut plan = args.spec.build_plan().unwrap();
    plan.commands.setup = vec![
        "setblock 10000 64 10000 minecraft:stone".to_string(),
    ];
    let error = HeavyServerHarness::run(args, plan, V770ServerProtocol)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("server.opaque_cells_encoded"));
    assert!(error.to_string().contains("installed opaque_cells=1"));
    assert!(error.to_string().contains("consumed opaque_cells=0"));
    assert!(!error.to_string().contains("chunk_payload_bytes=0"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn entity_runtime_streams_the_real_tick_population_to_the_wire() {
    let mut args = HeavyServerArgs::for_test(HeavyScenario::Entity);
    args.output = std::env::temp_dir().join(format!(
        "lodestone-heavy-server-entity-{}.jsonl",
        std::process::id()
    ));
    let plan = args.spec.build_plan().unwrap();
    let record = HeavyServerHarness::run(args, plan, V770ServerProtocol)
        .await
        .unwrap();
    assert!(record.requested.entities_spawned >= 1024);
    assert!(record.installed.entities_spawned >= record.requested.entities_spawned);
    assert!(record.consumed.entities_extracted >= record.requested.entities_spawned);
    assert_eq!(record.consumed.entities_drawn, record.consumed.entities_extracted);
    assert!(record.consumed.server_ticks > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn entity_runtime_fails_when_the_population_producer_is_removed() {
    let mut args = HeavyServerArgs::for_test(HeavyScenario::Entity);
    args.output = std::env::temp_dir().join(format!(
        "lodestone-heavy-server-entity-empty-{}.jsonl",
        std::process::id()
    ));
    let mut plan = args.spec.build_plan().unwrap();
    plan.commands.setup.clear();
    let error = HeavyServerHarness::run(args, plan, V770ServerProtocol)
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("world.entities_drawn"), "{message}");
    assert!(message.contains("installed entities_spawned=0"), "{message}");
    assert!(message.contains("consumed entities_extracted=0"), "{message}");
    assert!(!message.contains("chunk_payload_bytes=0"), "{message}");
}

#[tokio::test]
async fn runtime_counts_are_observed_at_each_boundary() {
    let args = HeavyServerArgs::for_test(HeavyScenario::Palette);
    let plan = args.spec.build_plan().unwrap();
    let record = HeavyServerHarness::run(args, plan, V770ServerProtocol)
        .await
        .unwrap();
    assert_eq!(record.requested.opaque_cells, 64);
    assert!(record.installed.opaque_cells >= record.requested.opaque_cells);
    assert!(record.consumed.opaque_cells > 0);
    assert!(record.consumed.opaque_cells < record.installed.opaque_cells);
    assert_eq!(record.consumed.sections, 9 * 24);
    assert!(record.consumed.chunk_payload_bytes > 0);
}

#[tokio::test]
async fn terrain_runtime_scenarios_reach_the_wire_through_their_own_source_counters() {
    for scenario in [
        HeavyScenario::Transparency,
        HeavyScenario::Light,
        HeavyScenario::Liquid,
    ] {
        let mut args = HeavyServerArgs::for_test(scenario);
        args.output = std::env::temp_dir().join(format!(
            "lodestone-heavy-server-{}-{}.jsonl",
            scenario.as_str(),
            std::process::id()
        ));
        let plan = args.spec.build_plan().unwrap();
        let record = HeavyServerHarness::run(args, plan, V770ServerProtocol)
            .await
            .unwrap();
        match scenario {
            HeavyScenario::Transparency => {
                assert!(record.requested.translucent_cells > 0);
                assert_eq!(record.installed.translucent_cells, record.requested.translucent_cells);
                assert_eq!(record.consumed.translucent_cells, record.requested.translucent_cells);
            }
            HeavyScenario::Light => {
                assert!(record.requested.light_emitters > 0);
                assert_eq!(record.installed.light_emitters, record.requested.light_emitters);
                assert_eq!(record.consumed.light_emitters, record.requested.light_emitters);
            }
            HeavyScenario::Liquid => {
                assert!(record.requested.liquid_cells > 0);
                assert_eq!(record.installed.liquid_cells, record.requested.liquid_cells);
                assert_eq!(record.consumed.liquid_cells, record.requested.liquid_cells);
            }
            _ => unreachable!("terrain runtime loop lists only terrain scenarios"),
        }
        assert_eq!(record.consumed.join_columns, record.requested.join_columns);
        assert!(record.consumed.chunk_payload_bytes > 0);
    }
}

#[tokio::test]
async fn terrain_runtime_fails_when_the_transparency_producer_is_removed() {
    let mut args = HeavyServerArgs::for_test(HeavyScenario::Transparency);
    args.output = std::env::temp_dir().join(format!(
        "lodestone-heavy-server-transparency-empty-{}.jsonl",
        std::process::id()
    ));
    let mut plan = args.spec.build_plan().unwrap();
    plan.commands.setup.clear();
    let error = HeavyServerHarness::run(args, plan, V770ServerProtocol)
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("server.translucent_cells_encoded"), "{message}");
    assert!(message.contains("installed translucent_cells=0"), "{message}");
    assert!(message.contains("consumed translucent_cells=0"), "{message}");
    assert!(!message.contains("chunk_payload_bytes=0"), "{message}");
}
