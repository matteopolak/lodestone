//! Drift guard for the compiled-reference simple-block survival census.

use lodestone_data::{block_states::StateId, block_survival};

const DUMP: &str = include_str!("support/block_survival_jvm.txt");

fn rows() -> impl Iterator<Item = (u32, [bool; 4])> + 'static {
    let mut lines = DUMP.lines();
    assert_eq!(lines.next(), Some("C 32366"));
    lines.map(|line| {
        let mut fields = line.split_ascii_whitespace();
        assert_eq!(fields.next(), Some("S"));
        let id = fields.next().unwrap().parse().unwrap();
        let values = std::array::from_fn(|_| match fields.next() {
            Some("0") => false,
            Some("1") => true,
            other => panic!("invalid capability value {other:?} in {line}"),
        });
        assert!(fields.next().is_none(), "extra field in {line}");
        (id, values)
    })
}

#[test]
fn committed_bitsets_match_every_compiled_reference_state() {
    assert_eq!(block_survival::STATE_COUNT, 32_366);
    let readers: [fn(StateId) -> bool; 4] = [
        block_survival::solid_render,
        block_survival::sturdy_up,
        block_survival::center_support_down,
        block_survival::fire_flammable,
    ];
    let mut count = 0;
    for (raw, expected) in rows() {
        let state = StateId::new(raw).expect("dump state id is in range");
        for (read, want) in readers.into_iter().zip(expected) {
            assert_eq!(read(state), want, "state id {raw}");
        }
        count += 1;
    }
    assert_eq!(count, block_survival::STATE_COUNT);
}

#[test]
fn controls_distinguish_full_partial_fluid_and_waterlogged_fuel() {
    let state = |name| StateId::from_state_str(name).expect("known state");
    let stone = state("minecraft:stone");
    assert_eq!(
        (
            block_survival::solid_render(stone),
            block_survival::sturdy_up(stone),
            block_survival::center_support_down(stone),
            block_survival::fire_flammable(stone),
        ),
        (true, true, true, false)
    );
    let fence = state("minecraft:oak_fence[east=false,north=false,south=false,waterlogged=false,west=false]");
    assert_eq!(
        (
            block_survival::solid_render(fence),
            block_survival::sturdy_up(fence),
            block_survival::center_support_down(fence),
            block_survival::fire_flammable(fence),
        ),
        (false, false, true, true)
    );
    let water = state("minecraft:water[level=0]");
    assert_eq!(
        (
            block_survival::solid_render(water),
            block_survival::sturdy_up(water),
            block_survival::center_support_down(water),
            block_survival::fire_flammable(water),
        ),
        (false, false, false, false)
    );
    let wet_fence = state("minecraft:oak_fence[east=false,north=false,south=false,waterlogged=true,west=false]");
    assert!(!block_survival::fire_flammable(wet_fence));
}
