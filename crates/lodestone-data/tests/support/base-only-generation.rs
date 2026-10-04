{
    assert_eq!(
        lodestone_data::block_states::STATE_COUNT,
        32_366,
        "This workflow only emits 26.2 tables. Regenerate the complete union with \
         python3 crates/lodestone-data/tools/behavior_union.py --runtime-install; \
         verify it with --runtime-check."
    );
}
