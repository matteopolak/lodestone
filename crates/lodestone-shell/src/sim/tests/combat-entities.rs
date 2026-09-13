use super::*;
#[test]
fn attack_strength_scale_ramps_to_full_over_five_ticks_unarmed() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    assert_eq!(
        sim.attack_strength_scale(),
        0.0,
        "a fresh player must start at zero strength, matching Player's bare int field"
    );
    for expected_ticks in 1..=5u32 {
        sim.step(1.0 / 20.0);
        let want = (expected_ticks as f32 / 5.0).min(1.0);
        let got = sim.attack_strength_scale();
        assert!(
            (got - want).abs() < 1e-6,
            "after {expected_ticks} ticks expected scale {want}, got {got}"
        );
    }
    // One tick past the delay: still clamped at 1.0, not overshooting.
    sim.step(1.0 / 20.0);
    assert_eq!(sim.attack_strength_scale(), 1.0);
}

/// A weapon's `minecraft:attack_speed` modifier (a sword's net `1.6`, per
/// vanilla's item data) must change the delay, not just the unarmed
/// default — this is the whole reason the delay reads a live
/// server-fed [`Attributes`] snapshot instead of a hardcoded constant.
/// `20.0 / 1.6 = 12.5` ticks, so one tick in gives `1.0 / 12.5 = 0.08`.
#[test]
fn attack_strength_delay_follows_a_reported_attack_speed_attribute() {
    use std::str::FromStr;
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let local = sim.local_player();
    let key = lodestone_model::Identifier::from_str("minecraft:attack_speed").unwrap();
    sim.write(|w| {
        w.entity_mut(local).insert(Attributes(vec![
            lodestone_model::EntityAttributeSnapshot {
                attribute: key,
                base: 1.6,
                modifiers: Vec::new(),
            },
        ]));
    });
    sim.step(1.0 / 20.0);
    let got = sim.attack_strength_scale();
    assert!(
        (got - 0.08).abs() < 1e-5,
        "a 1.6 attack-speed weapon should give scale 0.08 after one tick, got {got}"
    );
}

/// [`Sim::attack_entity`] must reset the ticker **immediately**, in the
/// same call, not on the next tick — vanilla's
/// `MultiPlayerGameMode.attack` calls `resetAttackStrengthTicker()`
/// synchronously right after `player.attack(entity)`.
#[test]
fn attacking_an_entity_resets_the_strength_ticker_immediately() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    // Reach full strength first, so the reset is unambiguous.
    for _ in 0..5 {
        sim.step(1.0 / 20.0);
    }
    assert_eq!(sim.attack_strength_scale(), 1.0);

    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));
    sim.begin_attack_live();

    assert_eq!(
        sim.attack_strength_scale(),
        0.0,
        "attacking an entity must reset the ticker before the next tick, not after it"
    );
}

// -- crit particles ------------------------------------------------------
//
// `Sim::maybe_spawn_crit_particles`, reached only through the real
// production entry point (`begin_attack_live`), never called directly —
// proving the wiring, not just the private helper in isolation.

/// Spawns a real, ingested entity (through the same `ClientEvent` path
/// production uses, not a hand-built ECS component set) at `feet + (2,
/// 0, 0)`, so it is both a valid attack target and, via [`EntityIndex`],
/// resolvable by [`Sim::maybe_spawn_crit_particles`].
fn spawn_crit_test_target(sim: &mut Sim, entity_id: i32, kind: &str) {
    let feet = sim.player().position;
    ingest(
        sim,
        lodestone_client::ClientEvent::EntitySpawned {
            entity_id,
            uuid: None,
            entity_type: kind.parse().expect("valid entity type key"),
            pos: lodestone_model::Vec3::new(feet.x + 2.0, feet.y, feet.z),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        },
    );
}

/// Charges the attack-strength ticker to full (5 ticks, unarmed) with
/// `sprint` held throughout — stepping is required for a sprint key to
/// reach [`MovementIntent`] at all, so the charge and the sprint intent
/// are established together rather than in two passes that could disagree
/// about which ticks actually ran. `Forward` is held alongside `Sprint`
/// because vanilla's own sprint gate requires forward movement intent —
/// holding the sprint key alone (watched failing) never sets
/// `MovementIntent::sprint`, the same gate `submerged_and_sprinting_
/// enters_the_swim_pose`'s existing setup already relies on.
fn reach_full_strength(sim: &mut Sim, sprint: bool) {
    if sprint {
        sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
        sim.input_mut(|i| i.set(lodestone_controller::Action::Sprint, true));
    }
    for _ in 0..5 {
        sim.step(1.0 / 20.0);
    }
    assert_eq!(
        sim.attack_strength_scale(),
        1.0,
        "test setup must reach full attack strength before the assertions below mean \
         anything"
    );
}

fn crit_particle_count(sim: &mut Sim) -> usize {
    sim.particles_mut(|p| p.engine_mut().particles().len())
}

/// The positive case: full strength, airborne (falling, not grounded),
/// not sprinting, not submerged, target is a `LivingEntity` — vanilla's
/// `canCriticalAttack` is satisfied on every
/// clause this port models, so the attack must spawn crit particles.
#[test]
fn a_full_strength_airborne_hit_on_a_living_target_spawns_crit_particles() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    spawn_crit_test_target(&mut sim, 77, "minecraft:pig");
    reach_full_strength(&mut sim, false);
    let local = sim.local;
    sim.write(|w| {
        let mut state = w.get_mut::<PhysicsState>(local).expect("local player");
        state.0.fall_distance = 3.0;
        state.0.on_ground = false;
    });

    let before = crit_particle_count(&mut sim);
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(77));
    sim.begin_attack_live();
    let after = crit_particle_count(&mut sim);

    assert!(
        after > before,
        "a full-strength airborne hit on a living target must spawn crit particles, \
         before={before} after={after}"
    );
}

/// **Negative control, watched failing.** With the identical setup above
/// except `on_ground = true`, vanilla's `!onGround` clause fails and no
/// particles must spawn — proving the positive test is not vacuously
/// green (e.g. from particles some *other* code path already emits).
#[test]
fn crit_particles_do_not_spawn_while_grounded() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    spawn_crit_test_target(&mut sim, 78, "minecraft:pig");
    reach_full_strength(&mut sim, false);
    let local = sim.local;
    sim.write(|w| {
        let mut state = w.get_mut::<PhysicsState>(local).expect("local player");
        state.0.fall_distance = 3.0;
        state.0.on_ground = true;
    });

    let before = crit_particle_count(&mut sim);
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(78));
    sim.begin_attack_live();
    let after = crit_particle_count(&mut sim);

    assert_eq!(
        after, before,
        "a grounded hit must not spawn crit particles even at full strength and \
         fall_distance > 0"
    );
}

/// **Negative control.** Sprinting fails vanilla's `!isSprinting` clause.
#[test]
fn crit_particles_do_not_spawn_while_sprinting() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    spawn_crit_test_target(&mut sim, 79, "minecraft:pig");
    reach_full_strength(&mut sim, true);
    let local = sim.local;
    sim.write(|w| {
        let mut state = w.get_mut::<PhysicsState>(local).expect("local player");
        state.0.fall_distance = 3.0;
        state.0.on_ground = false;
    });
    assert!(
        sim.movement_intent().sprint,
        "test setup must actually be sprinting, or this control tests nothing"
    );

    let before = crit_particle_count(&mut sim);
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(79));
    sim.begin_attack_live();
    let after = crit_particle_count(&mut sim);

    assert_eq!(
        after, before,
        "a sprinting hit must not spawn crit particles"
    );
}

/// **Negative control.** A dropped item is not a living entity
/// (vanilla's own can-crit check's living-entity clause) —
/// vanilla never plays a crit sparkle on a punched item stack.
#[test]
fn crit_particles_do_not_spawn_against_a_non_living_target() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    spawn_crit_test_target(&mut sim, 80, "minecraft:item");
    reach_full_strength(&mut sim, false);
    let local = sim.local;
    sim.write(|w| {
        let mut state = w.get_mut::<PhysicsState>(local).expect("local player");
        state.0.fall_distance = 3.0;
        state.0.on_ground = false;
    });

    let before = crit_particle_count(&mut sim);
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(80));
    sim.begin_attack_live();
    let after = crit_particle_count(&mut sim);

    assert_eq!(
        after, before,
        "a hit on a non-living entity must not spawn crit particles"
    );
}

/// **Negative control.** Below `fullStrengthAttack`'s `> 0.9F` threshold,
/// vanilla's outer gate in `Player.attack` never reaches
/// `canCriticalAttack` at all — this is the ticker axis, not the
/// fall/ground/sprint/water axis the other controls cover.
#[test]
fn crit_particles_do_not_spawn_below_full_attack_strength() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    spawn_crit_test_target(&mut sim, 81, "minecraft:pig");
    // One tick in: well under the 5-tick unarmed delay, so
    // `attack_strength_scale_at(0.5)` is nowhere near `0.9`.
    sim.step(1.0 / 20.0);
    assert!(sim.attack_strength_scale() < 0.9);
    let local = sim.local;
    sim.write(|w| {
        let mut state = w.get_mut::<PhysicsState>(local).expect("local player");
        state.0.fall_distance = 3.0;
        state.0.on_ground = false;
    });

    let before = crit_particle_count(&mut sim);
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(81));
    sim.begin_attack_live();
    let after = crit_particle_count(&mut sim);

    assert_eq!(
        after, before,
        "an attack well under full strength must not spawn crit particles"
    );
}

/// The geometric half of entity targeting: [`Sim::update_entity_target`]
/// must find a spawned entity the ray points straight at, and report it
/// by its server (`MinecraftEntityId`), never a `bevy_ecs::Entity`.
#[test]
fn update_entity_target_finds_a_spawned_entity_along_the_ray() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let feet = sim.player().position;
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::EntitySpawned {
            entity_id: 99,
            uuid: None,
            entity_type: "minecraft:pig".parse().expect("valid entity type key"),
            pos: lodestone_model::Vec3::new(feet.x + 2.0, feet.y, feet.z),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        },
    );
    // A horizontal ray at a height just above the pig's own feet — safely
    // inside any real pig hitbox's vertical span without needing to know
    // its exact height, and well below a human eye height (1.6), which
    // would sail clean over a pig-sized box on a perfectly level ray.
    let origin = [feet.x, feet.y + 0.1, feet.z];
    let dir = [1.0, 0.0, 0.0];
    sim.update_entity_target(origin, dir, None);
    assert_eq!(
        sim.entity_target(),
        Some(99),
        "the ray should find the spawned pig by its server entity id"
    );
}

/// An entity past [`ENTITY_REACH`] must not be targetable, even though it
/// is well within block [`REACH`] — vanilla's shorter entity-interaction
/// range, not the block one.
#[test]
fn update_entity_target_ignores_an_entity_beyond_entity_reach() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let feet = sim.player().position;
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::EntitySpawned {
            entity_id: 7,
            uuid: None,
            entity_type: "minecraft:pig".parse().expect("valid entity type key"),
            // Within block REACH (4.5) but past ENTITY_REACH (3.0).
            pos: lodestone_model::Vec3::new(feet.x + 4.0, feet.y, feet.z),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        },
    );
    // Same height convention as `update_entity_target_finds_a_spawned_entity_along_the_ray`
    // — this must fail on *reach*, not on the ray sailing over the box.
    let origin = [feet.x, feet.y + 0.1, feet.z];
    let dir = [1.0, 0.0, 0.0];
    sim.update_entity_target(origin, dir, None);
    assert_eq!(
        sim.entity_target(),
        None,
        "an entity beyond entity-interaction range must not be targetable"
    );
}

/// Spawn one entity of `entity_type` two blocks in front of the player and
/// return what [`Sim::update_entity_target`] resolves the view ray to.
///
/// The ray is the same one the two tests above use — horizontal, `+x`, from
/// just above the player's feet — so an item's 0.25-block box and a pig's
/// 0.9-block one are both crossed, and the only thing that can differ between
/// two calls is the entity type. That is the point: this helper exists so the
/// exclusion test below can be run with a *pickable* type as its control and
/// have nothing else move.
fn ray_target_for_type(entity_type: &str, entity_id: i32) -> Option<i32> {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let feet = sim.player().position;
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::EntitySpawned {
            entity_id,
            uuid: None,
            entity_type: entity_type.parse().expect("valid entity type key"),
            pos: lodestone_model::Vec3::new(feet.x + 2.0, feet.y, feet.z),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        },
    );
    let origin = [feet.x, feet.y + 0.1, feet.z];
    let dir = [1.0, 0.0, 0.0];
    sim.update_entity_target(origin, dir, None);
    sim.entity_target()
}

/// The owner's live kick, at the layer that caused it: the view ray must not
/// resolve to a dropped item or an experience orb.
///
/// Killing a mob spawns its drops and its orbs inside the hitbox the mob just
/// vacated, so before this fix the next left-click picked one of them and sent
/// an attack naming it. Vanilla's own server-side attack handling treats a
/// dropped item or an experience orb target as a protocol violation and
/// disconnects with `multiplayer.disconnect.invalid_entity_attacked` — the
/// reported "Attempting to attack an invalid entity". Vanilla never sends it
/// because vanilla's own is-pickable check is `false` for both and neither overrides it.
///
/// The pig arm is the control and it is load-bearing: it proves this ray does
/// cross a box at that position, so a `None` from the other two arms is the
/// type predicate doing its job rather than a mis-aimed fixture. Both arms are
/// collected before asserting, so a failure reports every type rather than
/// stopping at the first.
#[test]
fn the_view_ray_never_picks_a_dropped_item_or_an_experience_orb() {
    assert_eq!(
        ray_target_for_type("minecraft:pig", 51),
        Some(51),
        "control: a pig at this exact position must be targetable, or the two \
         exclusions below prove nothing"
    );

    let picked: Vec<&str> = ["minecraft:item", "minecraft:experience_orb"]
        .into_iter()
        .filter(|kind| ray_target_for_type(kind, 52).is_some())
        .collect();
    assert!(
        picked.is_empty(),
        "these types must never be picked — the server kicks the session for \
         attacking one: {picked:?}"
    );
}

/// [`entity_type_can_be_picked`] against the whole 26.2 entity-type census,
/// rather than against the handful of names the bug happened to involve.
///
/// The count is the drift guard. The predicate is a reduction over ten vanilla
/// `isPickable()` declaring classes, and a version bump that adds an entity type
/// lands it in exactly one of two buckets — the census's `is_living` column, or
/// this module's explicit non-living lists. A new *living* type moves this total
/// and the test names it; a new non-living one does not move it and stays
/// unpickable, which is vanilla's own default and cannot cause a kick.
///
/// The named rows are the ones that decide something the count cannot: two the
/// server kicks for, three arrow types whose exclusion comes from a tag rather
/// than from their own override, and a dragon that is living and still not
/// pickable.
#[test]
fn the_pick_predicate_matches_the_vanilla_entity_census() {
    use crate::interact::entity_type_can_be_picked;

    let key = |name: &str| -> lodestone_model::ResourceKey {
        name.parse().expect("valid entity type key")
    };

    let mut wrong = Vec::new();
    for (name, expected) in [
        // Rejected by vanilla's own server-side attack handling — these two are the reported kick.
        ("minecraft:item", false),
        ("minecraft:experience_orb", false),
        // Vanilla's own arrow-family is-pickable check falls back to its
        // projectile base's own redirectable-projectile tag test,
        // which no arrow type is in. So arrows are never pickable at all.
        ("minecraft:arrow", false),
        ("minecraft:spectral_arrow", false),
        ("minecraft:trident", false),
        // Non-redirectable projectiles fall to the same tag test.
        ("minecraft:snowball", false),
        ("minecraft:egg", false),
        // The three tag members that *are* redirectable.
        ("minecraft:fireball", true),
        ("minecraft:wind_charge", true),
        ("minecraft:breeze_wind_charge", true),
        // Living, plus the one living type that overrides back to `false`.
        ("minecraft:pig", true),
        ("minecraft:zombie", true),
        ("minecraft:player", true),
        ("minecraft:armor_stand", true),
        ("minecraft:ender_dragon", false),
        // One from each non-living pickable family.
        ("minecraft:oak_boat", true),
        ("minecraft:bamboo_raft", true),
        ("minecraft:hopper_minecart", true),
        ("minecraft:painting", true),
        ("minecraft:item_frame", true),
        ("minecraft:end_crystal", true),
        ("minecraft:interaction", true),
        ("minecraft:falling_block", true),
        ("minecraft:tnt", true),
        ("minecraft:shulker_bullet", true),
        // The `Entity` default, and a namespace the census cannot speak for.
        ("minecraft:area_effect_cloud", false),
        ("minecraft:text_display", false),
        ("minecraft:marker", false),
        ("someplugin:custom_mob", false),
    ] {
        if entity_type_can_be_picked(&key(name)) != expected {
            wrong.push(name);
        }
    }
    assert!(wrong.is_empty(), "misclassified entity types: {wrong:?}");

    let pickable = (0..lodestone_data::entity_types::TYPE_COUNT)
        .filter_map(|id| lodestone_data::entity_types::entity_type_name(id as i32))
        .filter(|name| entity_type_can_be_picked(&key(name)))
        .count();
    assert_eq!(
        pickable, 131,
        "the 26.2 census has 131 pickable entity types of {}; a change here \
         means a type moved between the living column and the explicit lists",
        lodestone_data::entity_types::TYPE_COUNT
    );
}

/// A server velocity event (`ClientEvent::EntityVelocity`) naming the local
/// player's own server entity id must overwrite `PlayerState.velocity` outright:
/// the reference behavior is an unconditional replacement, with no local-player
/// override. The generic `Velocity` component is not read for the local player,
/// so routing this event there would leave a server-applied hit motionless.
#[test]
fn server_sent_knockback_replaces_the_local_players_velocity() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    ingest(&mut sim, login_event(3));
    assert_eq!(
        sim.player().velocity,
        Vec3d::ZERO,
        "test setup: a fresh player starts at rest"
    );
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::EntityVelocity {
            entity_id: 3,
            velocity: lodestone_model::Vec3::new(1.0, 2.0, -3.0),
        },
    );
    assert_eq!(
        sim.player().velocity,
        Vec3d::new(1.0, 2.0, -3.0),
        "knockback naming our own id must land in PlayerState.velocity, \
         the field `player_physics` actually integrates"
    );
}

/// The swing is a **tick** state machine. Reading it across many sub-tick
/// frames must not advance it — the defect
/// `limb_swing_tracks_per_tick_travel_not_the_interpolation_gap` records for
/// the walk cycle, where a per-frame drive made the animation up to 3x too
/// fast and frame-rate dependent.
#[test]
fn swing_progress_is_tick_driven_not_frame_driven() {
    let mut sim = Sim::new(test_config());
    sim.swing_hand();
    sim.step(1.0 / 20.0); // one whole tick: the clock starts
    sim.step(1.0 / 20.0); // and advances once
    let after_two_ticks = sim.hand_swing_progress();

    // 200 sub-tick frames at 1 ms. `FrameClock` accumulates them, so a few
    // whole ticks *will* elapse across 200 ms — the claim is not "nothing
    // changes", it is that the change tracks elapsed *ticks*, so 200 tiny
    // frames advance the swing no further than the 4 ticks their total
    // duration contains.
    for _ in 0..200 {
        sim.step(0.001);
    }
    let after_frames = sim.hand_swing_progress();
    let ticks_elapsed = 4; // 200 ms / 50 ms
    let ceiling = after_two_ticks + (ticks_elapsed + 1) as f32 / 6.0;
    assert!(
        after_frames <= ceiling,
        "200 sub-tick frames advanced the swing to {after_frames}, past the {ceiling} \
         that {ticks_elapsed} ticks of elapsed time allows — the clock is being \
         driven per frame"
    );
}

/// Both consumers read the same clock, so the first-person arm and the
/// self-avatar's body can never disagree about where in the swing we are.
#[test]
fn the_third_person_body_swings_off_the_same_clock_as_the_arm() {
    let mut sim = Sim::new(test_config());
    sim.cycle_camera_type();
    sim.swing_hand();
    // Step to a tick where the swing is genuinely mid-arc, so `assert_eq` is
    // comparing something other than two zeroes.
    let mut arm = 0.0;
    for _ in 0..4 {
        sim.step(1.0 / 20.0);
        arm = sim.hand_swing_progress();
        if arm > 0.1 {
            break;
        }
    }
    assert!(arm > 0.1, "the swing should be mid-arc, got {arm}");
    let body = sim
        .third_person_body_state()
        .expect("third person is on")
        .anim
        .attack_anim;
    assert!(
        (body - arm).abs() < 1e-6,
        "the self-avatar's attack_anim ({body}) must match the arm's ({arm})"
    );
}

/// The local self-avatar is a synthetic draw rather than a tracked entity, so
/// its held stack must preserve the modern client-only `minecraft:item_model`
/// component while it is narrowed to the renderer's visual id.
#[test]
fn third_person_body_uses_the_selected_stack_item_model_for_its_main_hand() {
    let mut sim = Sim::new(test_config());
    sim.cycle_camera_type();
    let mut stack = lodestone_model::ItemStack::new(
        "minecraft:diamond_sword".parse().expect("valid gameplay item id"),
        1,
    );
    stack.components.item_model = Some("server:gun".parse().expect("valid visual item id"));
    let local = sim.local;
    sim.write(|world| {
        world
            .get_mut::<lodestone_ecs::SessionMenus>(local)
            .expect("local player has menus")
            .0
            .apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: 0,
                item: Some(stack),
            });
    });

    let body = sim
        .third_person_body_state()
        .expect("third-person body is enabled");
    assert_eq!(
        body.equipment
            .iter()
            .find(|(slot, _)| *slot == EquipmentSlot::MainHand)
            .map(|(_, id)| id.to_string())
            .as_deref(),
        Some("server:gun"),
        "the local avatar's hand resolves the item definition, not its vanilla gameplay id"
    );
}

/// The local avatar becomes a synthetic [`EntityDraw`](crate::entities::EntityDraw)
/// only after this state is made. Its held player head must therefore retain the
/// profile URL beside the visual item id; otherwise the third-person special-item
/// pass can only bind its static Steve fallback.
#[test]
fn third_person_body_retains_the_selected_heads_profile_skin_for_its_main_hand() {
    const URL: &str = "https://example.invalid/custom-head.png";
    const TEXTURES: &str =
        "eyJ0ZXh0dXJlcyI6eyJTS0lOIjp7InVybCI6Imh0dHBzOi8vZXhhbXBsZS5pbnZhbGlkL2N1c3RvbS1oZWFkLnBuZyJ9fX0=";

    let mut sim = Sim::new(test_config());
    sim.cycle_camera_type();
    let mut stack = lodestone_model::ItemStack::new(
        "minecraft:player_head".parse().expect("valid player-head item id"),
        1,
    );
    stack.components.profile = Some(lodestone_model::ItemProfile {
        name: Some("custom head".to_owned()),
        id: None,
        properties: vec![lodestone_model::ProfileProperty {
            name: "textures".to_owned(),
            value: TEXTURES.to_owned(),
            signature: None,
        }],
    });
    let local = sim.local;
    sim.write(|world| {
        world
            .get_mut::<lodestone_ecs::SessionMenus>(local)
            .expect("local player has menus")
            .0
            .apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: 0,
                item: Some(stack),
            });
    });

    let body = sim
        .third_person_body_state()
        .expect("third-person body is enabled");
    assert_eq!(
        body.equipment_skin
            .iter()
            .find(|(slot, _)| *slot == EquipmentSlot::MainHand)
            .map(|(_, skin)| skin.as_ref()),
        Some(URL),
        "the synthetic local-avatar draw must retain the held head's profile URL"
    );
}

/// A cached profile skin belongs to its UUID. A sessionless preview has no
/// active UUID and must not consume whichever account happened to publish
/// most recently — that was the cross-account skin flash on join.
#[test]
fn a_sessionless_body_does_not_consume_another_accounts_cached_model() {
