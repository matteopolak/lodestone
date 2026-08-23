//! Which F3 sub-mode each debug-line flag actually selects.
//!
//! The owner reported F3+B (hitboxes) and F3+G (chunk borders) both putting the
//! chunk-border grid on screen while the chat feedback named the right one. The
//! chat half is already gated at the key layer
//! (`app::tests::f3_b_and_f3_g_flip_their_atomic_and_push_chat_through_the_real_key_path`
//! drives the real `apply_key_outcome` and asserts on the real atomics), so the
//! only layer left unguarded was the *consumer*: whichever code reads those two
//! `Arc<AtomicBool>`s and picks a geometry producer. Nothing reached it, because
//! every existing debug-line gate installs a synthetic closure and so never
//! exercises the mapping at all.
//!
//! Two adjacent `bool`s transpose without a trace, and — unlike a numeric pair —
//! they coincide half the time by chance, so **a fixture that sets them to the
//! same value cannot see a transposition**. Every arm below therefore sets the
//! two flags to *different* values, and the fixture puts the entity in a
//! different chunk from the player so a swapped producer cannot land on
//! coincidentally similar geometry either.

use lodestone::entities::EntityDraw;
use lodestone::gpu::{DebugLineVertex, chunk_border_vertices, entity_hitbox_vertices, f3_overlay_vertices};

/// Colours `entity_hitbox_vertices` emits: white hitbox, cyan eye ray.
const HITBOX_COLOURS: [[f32; 4]; 2] = [[1.0, 1.0, 1.0, 1.0], [0.0, 1.0, 1.0, 1.0]];
/// Colours `chunk_border_vertices` emits: yellow column edge, blue section ring.
const BORDER_COLOURS: [[f32; 4]; 2] = [[1.0, 1.0, 0.0, 1.0], [0.25, 0.25, 1.0, 1.0]];

/// The player stands in chunk `(12, -5)`; the mob is at world origin, twelve
/// chunks away, so the two producers' geometry cannot overlap by accident.
const PLAYER: [f64; 3] = [200.5, 64.0, -73.5];
const MIN_Y: i32 = -64;
const HEIGHT: u32 = 384;

fn zombie() -> EntityDraw {
    EntityDraw {
        id: 1,
        type_path: std::sync::Arc::from("zombie"),
        variant_sheet: None,
        item: None,
        equipment: Vec::new(),
        equipment_dye: Vec::new(),
        equipment_trim: Vec::new(),
        wool: None,
        block_state: None,
        item_frame_rotation: 0,
        count: 1,
        foil: false,
        item_dyed_color: None,
        item_potion_color: None,
        feet: glam::Vec3::new(0.0, 64.0, 0.0),
        yaw: 0.0,
        head_yaw: 0.0,
        pitch: 0.0,
        scale: 1.0,
        anim: lodestone_render::entity_anim::AnimInput::default(),
        name_tag: None,
        hurt: false,
        item_use: None,
        main_arm_left: false,
        creeper_swelling: 0.0,
        swim_amount: 0.0,
        death_time: 0.0,
        on_fire: false,
        invisible: false,
        armor_stand: None,
        player_skin: None,
        experience_orb_value: None,
        cape_sway: (0.0, 0.0, 0.0),
        painting: None,
        firework: None,
        projectile_owner: None,
    }
}

fn carries(verts: &[DebugLineVertex], colours: [[f32; 4]; 2]) -> bool {
    verts
        .iter()
        .any(|v| colours.iter().any(|c| v.color == *c))
}

#[test]
fn each_f3_debug_flag_selects_its_own_producer() {
    let draws = [zombie()];

    // Positive controls: both producers really do emit geometry for this
    // fixture, so an "only one of them fired" assertion below is not satisfied
    // by a producer that emits nothing whatever the flag says.
    let hitbox_only_expected = entity_hitbox_vertices(&draws);
    let border_only_expected = chunk_border_vertices(PLAYER, MIN_Y, HEIGHT);
    assert!(
        !hitbox_only_expected.is_empty(),
        "control: the hitbox producer must emit geometry for a real census entity"
    );
    assert!(
        !border_only_expected.is_empty(),
        "control: the chunk-border producer must emit geometry for a real column"
    );

    // F3+B on, F3+G **off** — deliberately different, which is the whole point.
    let hitboxes_only = f3_overlay_vertices(&draws, PLAYER, MIN_Y, HEIGHT, true, false);
    assert_eq!(
        hitboxes_only.len(),
        hitbox_only_expected.len(),
        "F3+B alone must produce exactly the entity-hitbox geometry"
    );
    assert!(
        carries(&hitboxes_only, HITBOX_COLOURS),
        "F3+B alone produced no hitbox-coloured geometry"
    );
    assert!(
        !carries(&hitboxes_only, BORDER_COLOURS),
        "F3+B alone produced chunk-border-coloured geometry — the two flags are crossed"
    );

    // F3+G on, F3+B **off**.
    let borders_only = f3_overlay_vertices(&draws, PLAYER, MIN_Y, HEIGHT, false, true);
    assert_eq!(
        borders_only.len(),
        border_only_expected.len(),
        "F3+G alone must produce exactly the chunk-border geometry"
    );
    assert!(
        carries(&borders_only, BORDER_COLOURS),
        "F3+G alone produced no chunk-border-coloured geometry"
    );
    assert!(
        !carries(&borders_only, HITBOX_COLOURS),
        "F3+G alone produced hitbox-coloured geometry — the two flags are crossed"
    );

    // The geometry is genuinely disjoint, not merely differently coloured: the
    // mob is twelve chunks from the player, so no hitbox vertex can sit inside
    // the chunk column and no column vertex can sit on the mob.
    let in_column = |v: &DebugLineVertex| {
        (192.0..=208.0).contains(&v.position[0]) && (-80.0..=-64.0).contains(&v.position[2])
    };
    assert!(
        !hitboxes_only.iter().any(in_column),
        "F3+B geometry reached the player's chunk column"
    );
    assert!(
        borders_only.iter().all(in_column),
        "F3+G geometry left the player's chunk column"
    );

    // Both off draws nothing; both on is the concatenation, in that order.
    assert!(
        f3_overlay_vertices(&draws, PLAYER, MIN_Y, HEIGHT, false, false).is_empty(),
        "neither flag set must draw nothing"
    );
    let both = f3_overlay_vertices(&draws, PLAYER, MIN_Y, HEIGHT, true, true);
    assert_eq!(
        both.len(),
        hitbox_only_expected.len() + border_only_expected.len(),
        "both flags set must be the two producers' concatenation"
    );
}
