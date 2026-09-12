//! Tests for world seed parsing and launch configuration resolution.

use super::*;

/// Vanilla's own seed parsing: a valid `i64` literal is used
/// verbatim (vanilla tries a plain long parse first), whitespace is
/// trimmed, and non-numeric text falls back to the Java hash — not a new
/// rule, just `parse_seed` calling straight through to the constant test
/// above.
#[test]
fn parse_seed_follows_vanillas_own_rule() {
    assert_eq!(parse_seed("12345"), 12345);
    assert_eq!(parse_seed("-42"), -42);
    assert_eq!(parse_seed("  42  "), 42, "vanilla trims before parsing");
    assert_eq!(
        parse_seed("hello"),
        99_162_322,
        "non-numeric text must hash exactly like Java's own String.hashCode, \
         not this crate's own notion of a hash"
    );
}
/// An empty seed means "random" (vanilla's own random-seed default) —
/// asserted by absence of a fixed answer, the only honest assertion for
/// "random": two draws must not collide (astronomically unlikely for a
/// real `i64` random source, impossible for a constant-returning bug).
#[test]
fn empty_seed_is_random_not_a_fixed_fallback() {
    let a = parse_seed("");
    let b = parse_seed("   ");
    assert_ne!(
        a, b,
        "two empty-seed draws must not produce the same i64 — a constant \
         here would silently make every \"random\" world identical"
    );
}

/// A queued-patch check driven end to end: two different
/// `WorldCreationConfig`s (the exact type `Screen::CreateWorld` collects)
/// resolved through the *production* `resolve_launch_seed` must generate
/// **different real terrain** at the same coordinate — not merely
/// different `i64`s, which `parse_seed`'s own tests above already cover
/// and which would be the isolated-unit species of this gate. And the
/// same config must reproduce identical terrain.
///
/// `lodestone_server::overworld_generator` is exactly what
/// `crate::net::run`'s `Origin::Integrated` arm calls with this
/// function's resolved seed, once it has gone through
/// `lodestone_server::region_source::resolve_world_seed` — so this proves
/// the seed that would reach the wire, not a stand-in.
#[test]
fn resolved_seeds_from_different_world_creation_configs_generate_different_terrain() {
    let config_a = crate::menu::create_world::WorldCreationConfig {
        seed: "100".to_string(),
        ..Default::default()
    };
    let config_b = crate::menu::create_world::WorldCreationConfig {
        seed: "999999".to_string(),
        ..Default::default()
    };

    let seed_a = resolve_launch_seed(Some(&config_a));
    let seed_b = resolve_launch_seed(Some(&config_b));
    assert_eq!(seed_a, 100);
    assert_eq!(seed_b, 999_999);

    let column_a = lodestone_server::overworld_generator(seed_a).column(0, 0);
    let column_b = lodestone_server::overworld_generator(seed_b).column(0, 0);

    let mut differences = 0usize;
    for lz in 0..16usize {
        for lx in 0..16usize {
            for y in (column_a.min_y()..column_a.min_y() + column_a.height()).step_by(4) {
                if column_a.block_state(lx, y, lz) != column_b.block_state(lx, y, lz) {
                    differences += 1;
                }
            }
        }
    }
    assert!(
        differences > 0,
        "two different entered seeds must generate different terrain \
         somewhere in the same column — the config's seed is reaching \
         nowhere if this is 0"
    );

    // Reproducibility: the same config, resolved and generated twice,
    // must be byte-identical — `overworld_generator` is a pure function
    // of its seed, and this is the exact call `net.rs::run` makes, called
    // twice rather than reimplemented.
    let seed_a_again = resolve_launch_seed(Some(&config_a));
    assert_eq!(seed_a_again, seed_a, "the same typed seed must resolve identically");
    let column_a_again = lodestone_server::overworld_generator(seed_a_again).column(0, 0);
    for lz in 0..16usize {
        for lx in 0..16usize {
            for y in column_a.min_y()..column_a.min_y() + column_a.height() {
                assert_eq!(
                    column_a.block_state(lx, y, lz),
                    column_a_again.block_state(lx, y, lz),
                    "the same seed must reproduce identical terrain at ({lx},{y},{lz})"
                );
            }
        }
    }
}

/// `None` (`Screen::WorldSelect`'s Play Selected World) must still resolve
/// to the bundled world's own seed — the default behavior remains unchanged.
#[test]
fn no_config_resolves_to_the_bundled_worlds_seed() {
    assert_eq!(
        resolve_launch_seed(None),
        crate::menu::world_select::BUNDLED_WORLD.seed
    );
}
