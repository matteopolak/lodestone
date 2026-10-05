//! The production 26.3 Overworld front: bridges, shaped terrain and decoration over the real engine.

use lodestone_worldgen::terrain263::{HEIGHT, MIN_Y, Terrain263};

#[test]
fn every_decorator_state_has_a_canonical_counterpart() {
    let t = Terrain263::new(42).expect("builds");
    assert_eq!(t.unmapped_states(), 0);
    // The two bridges are inverse on every state the canonical census shares.
    for s in 0..t.env().blocks.state_count() as u16 {
        let canon = t.canonical(s);
        assert_eq!(t.canonical(t.feature_state(canon)), canon);
    }
}

#[test]
fn shaped_terrain_is_deterministic_and_seed_dependent() {
    let a = Terrain263::new(42).unwrap();
    let b = Terrain263::new(42).unwrap();
    let c = Terrain263::new(43).unwrap();
    let (sa, sb, sc) = (a.shaped(3, -2), b.shaped(3, -2), c.shaped(3, -2));
    assert_eq!(sa.chunk.states.len(), (256 * HEIGHT) as usize);
    assert_eq!(sa.chunk.states, sb.chunk.states);
    assert_ne!(sa.chunk.states, sc.chunk.states, "control: another seed must change the terrain");
    assert!(sa.chunk.states.iter().any(|&s| s != a.env().known.air));
    assert_eq!(sa.biomes.min_quart_y, MIN_Y >> 2);
}

#[test]
fn decoration_writes_land_and_replay_deterministically() {
    let t = Terrain263::new(42).unwrap();
    let writes = t.decorate_source((0, 0), &mut |_, _| None);
    assert!(!writes.is_empty(), "a decorated chunk changes blocks");
    assert_eq!(writes, t.decorate_source((0, 0), &mut |_, _| None));
    assert!(writes.iter().all(|&(x, _, z, _)| (-16..32).contains(&x) && (-16..32).contains(&z)));
}
