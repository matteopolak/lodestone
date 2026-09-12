use super::*;

#[test]
fn chunk_dirty_signal_reschedules_a_loaded_column() {
    // A `ChunkLoaded`/`NetUpdate::Chunk { x, z }` signal must re-mesh the
    // column it names (the §12.24 dirty-region trigger), so the live-world
    // swap is a source change, not new plumbing.
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    assert_eq!(sim.pending_meshes(), 0, "drained to a clean slate");
    let pos = *sim
        .chunk_world()
        .read()
        .iter()
        .next()
        .expect("local world has a column")
        .0;
    let (cx, cz) = (pos.x, pos.z);
    sim.mark_column_dirty(cx, cz);
    assert!(
        sim.pending_meshes() > 0,
        "the loaded column was re-scheduled"
    );
}

#[test]
fn chunk_arrival_also_remeshes_its_loaded_neighbours() {
    // A section's geometry depends on its whole 3×3×3 neighbourhood, so a
    // column meshed before its neighbour loaded baked its seam against air —
    // which is what puts a falling water "wall" at every chunk border. The
    // arrival signal must therefore dirty the eight loaded neighbours too.
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let pos = *sim
        .chunk_world()
        .read()
        .iter()
        .next()
        .expect("local world has a column")
        .0;
    // Pick a column with at least one loaded horizontal neighbour.
    let (cx, cz) = (pos.x, pos.z);
    let neighbours: Vec<(i32, i32)> = (-1..=1)
        .flat_map(|dx| (-1..=1).map(move |dz| (dx, dz)))
        .filter(|&(dx, dz)| (dx, dz) != (0, 0))
        .map(|(dx, dz)| (cx + dx, cz + dz))
        .filter(|&(nx, nz)| sim.chunk_world().contains_column(nx, nz))
        .collect();
    assert!(
        !neighbours.is_empty(),
        "fixture must have a loaded neighbour, else this asserts nothing"
    );

    sim.on_column_arrived(cx, cz);
    // `heal_dirty_columns` is an `Update` system now; run the schedule the way
    // `Sim::step` does rather than calling a method. `DIRTY_COLUMN_BUDGET` is
    // 4 and the fixture has up to 8 loaded neighbours, so drive it until the
    // dirty set is empty.
    while !sim.terrain(|t| t.dirty_columns.is_empty()) {
        sim.ecs().write().run_schedule(lodestone_ecs::Update);
    }
    let _ = neighbours.len();
    let meshed: HashSet<(i32, i32)> = sim
        .drain_all_meshes()
        .into_iter()
        .map(|m| (m.key.cx, m.key.cz))
        .chain(sim.drain_removals().into_iter().map(|k| (k.cx, k.cz)))
        .collect();

    assert!(meshed.contains(&(cx, cz)), "the arriving column was meshed");
    for n in &neighbours {
        assert!(
            meshed.contains(n),
            "loaded neighbour {n:?} was not re-meshed — its seam stays baked \
             against air (the chunk-border water wall)"
        );
    }
}

#[test]
fn neighbour_remesh_skips_columns_that_are_not_loaded() {
    // The control for the test above: queueing absent columns would mesh
    // nothing, log a drop, and let "every arrival dirties 8 neighbours" pass
    // without any of them being real.
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.on_column_arrived(9999, 9999);
    assert!(
        sim.terrain(|t| t.dirty_columns.is_empty()),
        "no neighbour of an out-of-world column is loaded, so none is queued"
    );
}

#[test]
fn chunk_dirty_signal_ignores_an_absent_column() {
    // Columns we don't hold (e.g. before the live world source is wired in)
    // must be a no-op, never a panic or spurious work.
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.mark_column_dirty(9999, 9999);
    assert_eq!(sim.pending_meshes(), 0, "absent column schedules nothing");
}

#[test]
fn placing_against_a_face_adds_a_block() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let feet = sim.player().position;
    // Target a floor block a few blocks away (clear of the player AABB),
    // place on its top face.
    let bx = feet.x.floor() as i32 + 3;
    let bz = feet.z.floor() as i32;
    let s = crate::worldgen::surface_height(bx, bz);
    sim.set_target(Some(crate::raycast::RayHit::face_center(
        [bx, s, bz],
        [0, 1, 0],
    )));
    {
        let store = sim.chunk_world();
        let world = store.read();
        let view = WorldCollision::new(&world);
        assert_eq!(view.block_at(bx, s + 1, bz), id::AIR, "cell starts empty");
    }
    assert!(sim.place_block(), "should place onto the top face");
    let store = sim.chunk_world();
    let world = store.read();
    let view = WorldCollision::new(&world);
    assert_ne!(view.block_at(bx, s + 1, bz), id::AIR, "block now present");
}

#[test]
fn cannot_place_inside_the_player() {
    let mut sim = Sim::new(test_config());
    for _ in 0..20 {
        sim.step(1.0 / 20.0);
    }
    let feet = sim.player().position;
    // Target the block under the feet, whose top face is where the player
    // stands — placing there would clip the player, so it must be refused.
    sim.set_target(Some(crate::raycast::RayHit::face_center(
        [
            feet.x.floor() as i32,
            feet.y.floor() as i32 - 1,
            feet.z.floor() as i32,
        ],
        [0, 1, 0],
    )));
    assert!(!sim.place_block(), "placing inside the player is refused");
}

/// A real walking player must actually accumulate walk distance and ease the
/// amplitude up, and **only the render
/// camera** may see the result.
///
/// The corridor is not decoration. The offline world is real generated
/// terrain (`lodestone-worldgen`), the player spawns on a slope, and walking
/// north walls them out after ~0.2 blocks — `distance_walked_scales_with_the`
/// speed test above demonstrated. A bob gate run against a
/// walled-in player reads `walk_phase: -0.0, bob: 0.0` and asserts nothing,
/// which is the *precondition* species of vacuous test.
#[test]
fn walking_accumulates_a_real_bob_that_only_the_render_camera_sees() {
    let mut sim = Sim::new(test_config());
    // Player spawns at (0.5, feet, 0.5) facing north (-Z, yaw 180).
    let feet_y = sim.player().position.y.floor() as i32;
    for dz in -25..=1 {
        for dx in -1..=1 {
            sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
            sim.set_block_world([dx, feet_y, dz], id::AIR);
            sim.set_block_world([dx, feet_y + 1, dz], id::AIR);
            sim.set_block_world([dx, feet_y + 2, dz], id::AIR);
        }
    }
    // Settle on the fresh floor: while airborne `updateBob`'s `onGround` gate
    // holds the amplitude at zero, so a gate that never lands measures the
    // fall rather than the walk.
    for _ in 0..20 {
        sim.step(1.0 / 20.0);
    }
    assert!(
        sim.player().on_ground,
        "precondition: the player must be standing before the walk starts"
    );
    let still = sim.bob_frame();
    assert_eq!(still.bob, 0.0, "a settled, still player has no amplitude");
    assert_eq!(
        sim.render_camera(1.0).position,
        sim.camera(1.0).position,
        "and with no bob the two cameras are bit-identical, not merely close"
    );

    let start = sim.player().position;
    sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
    for _ in 0..30 {
        sim.step(1.0 / 20.0);
    }
    let travelled = (sim.player().position.z - start.z).abs();
    assert!(
        travelled > 1.0,
        "precondition: the corridor must let the player actually walk; only \
         {travelled:.3} blocks covered, so the bob below would be measuring a \
         walled-in player"
    );

    let walking = sim.bob_frame();
    assert!(
        walking.bob > 0.02,
        "the amplitude must ease up from real movement, got {}",
        walking.bob
    );
    assert!(
        walking.bob <= 0.1 + 1e-6,
        "and must never exceed vanilla's 0.1 ceiling, got {}",
        walking.bob
    );
    // `walkDist` is `distance * 0.6` accumulated, then negated, so a metre of
    // travel is well over half a unit of phase.
    assert!(
        walking.walk_phase.abs() > 0.5,
        "the stride phase must advance, got {}",
        walking.walk_phase
    );

    // The half that would be a gameplay bug rather than a visual one.
    // `Self::camera` is the block-targeting ray origin *and* the audio
    // listener; vanilla bobs neither, because its bob is folded into the
    // projection matrix and `getPickRay` never reads that.
    assert_ne!(
        sim.render_camera(1.0).position,
        sim.camera(1.0).position,
        "the drawn camera must bob"
    );

    // And the option zeroes the frame outright rather than scaling it, so
    // `bobbed_camera` short-circuits and the two cameras are byte-equal again.
    sim.set_view_bobbing(false);
    assert_eq!(sim.bob_frame(), crate::camera_rig::BobFrame::default());
    assert_eq!(
        sim.render_camera(1.0).position,
        sim.camera(1.0).position,
        "with View Bobbing off, render_camera must be bit-identical to camera"
    );
    assert_eq!(sim.render_camera(1.0).pitch, sim.camera(1.0).pitch);
    // Control: turning it back on restores the difference, so the equality
    // above is the option working and not the walk having decayed.
    sim.set_view_bobbing(true);
    assert_ne!(
        sim.render_camera(1.0).position,
        sim.camera(1.0).position,
        "control failed: the bob is gone regardless of the option, so the \
         equality above proves nothing about the option"
    );
}

/// The camera-side half of `bobHurt`: a local-player damage report must reach
/// the interpolated bob frame with its direction, and must **survive View
/// Bobbing being off** — vanilla's `bobHurt` is unconditional
///, only `bobView` is gated on the option.
///
/// The net-apply feed (`ClientEvent::EntityHurtAnimation` naming the local
/// player's own id → [`Sim::on_local_player_hurt`]) is live now — `net.rs`'s
/// `forward` produces `NetUpdate::HurtAnimation` and `net_apply` filters it
/// against `server_entity_id()`. This test still drives the hook directly, which
/// keeps it hermetic. What it pins is the *camera's* contract:
/// the countdown and the wire `yaw` (90° here, a side hit — a frontal hit is
/// `hurtDir 0`, the pure-roll case, see `render_camera`) both reach the frame,
/// and the option must not mute them.
#[test]
fn local_player_hurt_reaches_the_bob_frame_and_survives_view_bobbing_off() {
    let mut sim = Sim::new(test_config());
    // Precondition: a never-hit player has no flash and no direction.
    assert!(sim.bob_frame().hurt <= 0.0, "no flash before any hit");
    assert_eq!(sim.bob_frame().hurt_dir_degrees, 0.0);

    sim.on_local_player_hurt(90.0);
    let hurt = sim.bob_frame();
    assert!(hurt.hurt > 0.0, "a fresh hit must be flashing");
    assert_eq!(hurt.hurt_dir_degrees, 90.0, "the wire yaw must survive");

    // Only the walk terms are gated on the option; the tilt is not.
    sim.set_view_bobbing(false);
    let off = sim.bob_frame();
    assert_eq!(off.walk_phase, 0.0, "the walk terms must still be muted");
    assert_eq!(off.bob, 0.0, "the walk terms must still be muted");
    assert!(off.hurt > 0.0, "bobHurt must not be muted by the option");
    assert_eq!(off.hurt_dir_degrees, 90.0);

    // The countdown is driven by the 20 Hz tick, like `LivingEntity.tick`'s.
    sim.step(1.0 / 20.0);
    assert!(
        sim.bob_frame().hurt < off.hurt,
        "the tilt must count down one tick at a time"
    );

    // `render_camera` still passes a zero strength, and that is now a *routing*
    // fact rather than a hold: `bobbed_camera` cannot carry roll, so the tilt
    // travels the eye-space seam instead. The camera's own pitch is therefore
    // untouched by the flash — asserted, because a future "fix" that smeared the
    // roll into pitch would look like progress and would be wrong.
    sim.set_view_bobbing(true);
    assert_eq!(
        sim.render_camera(1.0).pitch,
        sim.camera(1.0).pitch,
        "the tilt must not be smeared into the camera's pitch"
    );
}

/// The hop the test above used to call the missing one: a local-player damage
/// report must reach an **actual eye-space matrix**, and the accessibility option
/// must be able to switch it off.
///
/// This is the gate that catches the defect this feature spent months in: every
/// piece — the countdown, the direction, the quartic easing, the option — was
/// built and unit-tested, and the composed transform handed to the renderer was a
/// hard-coded identity. Asserting on `bob_frame().hurt` cannot see that; asserting
/// on the matrix can.
///
/// The magnitude is predicted rather than compared for inequality: at `hurt == 8`
/// the tilt is `-14·sin(0.4096π) = -13.03°`, whose matrix entries carry
/// `sin(13.03°) = 0.2255`. A tolerance of `0.01` therefore separates "the tilt
/// arrived" from "something moved" by more than twenty times.
#[test]
fn a_local_player_hit_reaches_a_real_eye_space_matrix() {
    let mut sim = Sim::new(test_config());
    assert_eq!(
        sim.damage_tilt_eye_transform().to_cols_array(),
        glam::Mat4::IDENTITY.to_cols_array(),
        "an unhurt player's transform must be exactly the identity"
    );

    sim.on_local_player_hurt(0.0);
    // Two ticks in, `hurt` is 8, which is close to the quartic peak.
    sim.step(1.0 / 20.0);
    sim.step(1.0 / 20.0);
    let frame = sim.bob_frame();
    // Recomputed here from the jar's constants rather than read back out of the
    // implementation: `-hurt' * 14`, where `hurt' = sin(t^4 * PI)`, `t = hurt/10`.
    let t = frame.hurt / 10.0;
    let expected_degrees = -14.0 * (t.powi(4) * std::f32::consts::PI).sin();
    let m = sim.damage_tilt_eye_transform();
    // A head-on hit is pure roll about eye +Z, so eye-space up moves in x by
    // exactly `-sin(tilt)`.
    let up = m.transform_vector3(glam::Vec3::Y);
    let predicted_x = -expected_degrees.to_radians().sin();
    assert!(
        (up.x - predicted_x).abs() < 0.01,
        "up moved to {up:?}; a {expected_degrees} degree roll predicts x = {predicted_x}"
    );
    assert!(
        up.x.abs() > 0.2,
        "precondition: the tilt near its peak is a fifth of a unit, not noise"
    );

    // The accessibility option is a real off switch, all the way through the sim.
    sim.set_damage_tilt_strength(0.0);
    let off = sim.damage_tilt_eye_transform().transform_vector3(glam::Vec3::Y);
    assert!(
        off.x.abs() < 1e-6,
        "a zero Damage Tilt strength must leave the matrix inert, got {off:?}"
    );
}

/// End-to-end: `Sim::spyglass_scoping`'s two halves
/// (`Self::using_item` and the held-item identity check) have to actually
/// reach `Self::render_camera`'s FOV, not just exist. Predicts the *exact*
/// FOV from `lodestone_render::spyglass_fov_modifier`'s tested `0.1`
/// constant rather than asserting only that the number changed — a wrong
/// multiplier would still pass a same-direction-only check.
#[test]
fn spyglass_scoping_zooms_the_render_camera_by_exactly_a_tenth() {
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    let base_fov = sim.render_camera(1.0).fov_y_degrees;
    assert_eq!(
        base_fov,
        crate::camera_rig::FOV_Y_DEGREES,
        "precondition: an empty hand must not zoom at all"
    );

    give_main_hand_item(&mut sim, "minecraft:spyglass");
    sim.use_item_live();
    let zoomed_fov = sim.render_camera(1.0).fov_y_degrees;
    assert_eq!(
        zoomed_fov,
        base_fov * lodestone_render::spyglass_fov_modifier(true),
        "a held, in-use spyglass must scale the FOV by exactly vanilla's 0.1 \
         override, not merely reduce it"
    );
    assert!(
        (zoomed_fov - 7.0).abs() < 1e-6,
        "70 degrees * 0.1 is 7.0 exactly; got {zoomed_fov}"
    );

    // -- negative control -------------------------------------------------
    // Using a non-spyglass item must not zoom, proving the assertions above
    // test the item's identity and not merely "is using any item".
    sim.end_use_live();
    give_main_hand_item(&mut sim, "minecraft:bow");
    sim.use_item_live();
    assert_eq!(
        sim.render_camera(1.0).fov_y_degrees,
        base_fov,
        "using a bow must not zoom — only a spyglass does"
    );

    // And releasing the spyglass must drop the zoom back to base, so the
    // wiring is proven live rather than latched permanently on the first
    // press.
    sim.end_use_live();
    give_main_hand_item(&mut sim, "minecraft:spyglass");
    assert_eq!(
        sim.render_camera(1.0).fov_y_degrees,
        base_fov,
        "holding a spyglass without using it must not zoom"
    );
}

/// The walk bob must reach the projection **at the reference magnitude and
/// axes**, driven by a real walking `Sim`.
///
/// # Why the existing gates could not have caught a wrong amplitude
///
/// Every other bob gate *supplies its own* `BobFrame`: the unit tests and
/// `tests/view_bob_pixels.rs` hand `ViewBob::tick`/`bobbed_camera` numbers
/// they chose, so they prove the arithmetic and can say nothing about whether
/// `Sim` feeds it realistic ones. That is `CLAUDE.md`'s *world* species —
/// the flaw would live in the input data and be invisible in the test source.
/// So step 1 here pins the **inputs** against vanilla's own walk speed,
/// measured from the player's position and not read back out of the bob.
///
/// # Why the far point is the discriminator
///
/// The bob is a translation *and* two rotations. A point at infinity is
/// unaffected by translation, so its screen displacement is the **nod alone**
/// — a nod-free bob moves it exactly `0.0` px. That is the separation
/// `docs/view-bobbing.md` records the chest-bbox pixel gate cannot make (its
/// +8.50 px is within 0.2 px of the +8.31 a nod-free bob gives). Conversely
/// the far point's *horizontal* displacement must stay at zero: the sway is a
/// translation and cannot move infinity, and the roll is deliberately dropped
/// by the fold, so any yaw leaking out of `bobbed_camera` shows up here.
///
/// The near point then carries the translation, and the two axes are
/// distinguishable by *shape* as well as size: the dip is rectified
/// (`-|cos|`, one-way) while the sway is a full sine (both ways). A gate that
/// only asked "did the frame change" passes on a bob with the wrong
/// amplitude, the wrong phase or the wrong axis; every number below is
/// predicted from vanilla's own walk-bob render pass's constants before it is measured.
#[test]
fn the_walk_bob_reaches_the_projection_at_vanillas_own_magnitude_and_axis() {
    /// Vanilla's walking speed, blocks per tick: `4.317 m/s / 20`.
    const WALK_BLOCKS_PER_TICK: f32 = 0.2159;
    /// Vanilla's own player-bob update's `Math.min(0.1F, ...)` ceiling,
    /// which a walking player saturates.
    const BOB_CEILING: f32 = 0.1;
    /// The nominal viewport the pixel predictions below are stated for.
    const VIEW_W: f32 = 1920.0;
    const VIEW_H: f32 = 1080.0;
    const ASPECT: f32 = VIEW_W / VIEW_H;

    let mut sim = Sim::new(test_config());
    // The corridor is a precondition, not decoration — see
    // `walking_accumulates_a_real_bob_that_only_the_render_camera_sees`.
    // Longer than that one's because this walks for ~5 s.
    let feet_y = sim.player().position.y.floor() as i32;
    for dz in -60..=1 {
        for dx in -1..=1 {
            sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
            sim.set_block_world([dx, feet_y, dz], id::AIR);
            sim.set_block_world([dx, feet_y + 1, dz], id::AIR);
            sim.set_block_world([dx, feet_y + 2, dz], id::AIR);
        }
    }
    for _ in 0..20 {
        sim.step(1.0 / 20.0);
    }
    assert!(sim.player().on_ground, "precondition: standing before the walk");

    // --- 1. The inputs. Vanilla's walk speed, vanilla's ceiling. ---------
    sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
    for _ in 0..30 {
        sim.step(1.0 / 20.0);
    }
    let before = sim.player().position;
    let phase_before = sim.bob_frame().walk_phase;
    sim.step(1.0 / 20.0);
    let moved = ((sim.player().position.x - before.x) as f32)
        .hypot((sim.player().position.z - before.z) as f32);
    assert!(
        (moved - WALK_BLOCKS_PER_TICK).abs() < 2e-3,
        "precondition: the player must be walking at vanilla's real speed, \
         not some fixture crawl — {moved:.5} blocks/tick against \
         {WALK_BLOCKS_PER_TICK}"
    );
    let settled = sim.bob_frame();
    assert!(
        (settled.bob - BOB_CEILING).abs() < 1e-4,
        "a walking player saturates `min(0.1, speed)`; got {}",
        settled.bob
    );
    // `LocalPlayer.move`: `addWalkedDistance(length(dx, dz) * 0.6)`, negated
    // by `getBackwardsInterpolatedWalkDistance`. Compared against `moved`,
    // which came from the position and not from the bob.
    let advance = phase_before - settled.walk_phase;
    assert!(
        (advance - moved * 0.6).abs() < 2e-4,
        "the stride phase must advance by exactly 0.6x the distance actually \
         travelled: {advance:.6} against {:.6}",
        moved * 0.6
    );

    // --- 2. The pixels, sampled at frame rate rather than tick rate. -----
    // 60 fps so the partial-tick interpolation is exercised and the sampling
    // lands within 0.07 rad of the nod's peak, which is what lets the
    // magnitude assertion below be tight.
    let screen = |c: &Camera, w: glam::Vec3| {
        let clip = c.view_projection() * w.extend(1.0);
        (
            (1.0 + clip.x / clip.w) * 0.5 * VIEW_W,
            (1.0 - clip.y / clip.w) * 0.5 * VIEW_H,
        )
    };
    let (mut far_dy_lo, mut far_dy_hi) = (f32::MAX, f32::MIN);
    let (mut far_dx_lo, mut far_dx_hi) = (f32::MAX, f32::MIN);
    let (mut near_dx_lo, mut near_dx_hi) = (f32::MAX, f32::MIN);
    let (mut near_dy_lo, mut near_dy_hi) = (f32::MAX, f32::MIN);
    for _ in 0..90 {
        sim.step(1.0 / 60.0);
        let cam = sim.camera(ASPECT);
        let bobbed = sim.render_camera(ASPECT);
        // **Both probes sit on `cam.forward()`, not on `-Z`.** They differ
        // only in distance, so the far one is the near one with the
        // translation's parallax divided away.
        //
        // Deriving the direction from the same expression the draw uses is
        // load-bearing, per `CLAUDE.md` — the offline spawn pitch is `10`,
        // not `0`, and a probe placed naively down `-Z` sits 10 deg above the
        // view centre. A pitch change of `t` moves a point at angle `a` by
        // `sec^2(a)/tan(fov/2)`, so that probe read **6.93 px** where the
        // on-axis prediction is 6.73: a 3% error, in the direction that looks
        // like the bob being slightly too strong. Chasing it as a code defect
        // is exactly the trap of restating a constant instead of deriving it.
        let far = cam.position + cam.forward() * 4096.0;
        let near = cam.position + cam.forward() * 3.0;
        for (p, dx_lo, dx_hi, dy_lo, dy_hi) in [
            (far, &mut far_dx_lo, &mut far_dx_hi, &mut far_dy_lo, &mut far_dy_hi),
            (near, &mut near_dx_lo, &mut near_dx_hi, &mut near_dy_lo, &mut near_dy_hi),
        ] {
            let (bx, by) = screen(&bobbed, p);
            let (sx, sy) = screen(&cam, p);
            *dx_lo = dx_lo.min(bx - sx);
            *dx_hi = dx_hi.max(bx - sx);
            *dy_lo = dy_lo.min(by - sy);
            *dy_hi = dy_hi.max(by - sy);
        }
    }
    let box_of = |dx: (f32, f32), dy: (f32, f32)| {
        format!("dx [{:.3}, {:.3}] dy [{:.3}, {:.3}] px", dx.0, dx.1, dy.0, dy.1)
    };
    let far_box = box_of((far_dx_lo, far_dx_hi), (far_dy_lo, far_dy_hi));
    let near_box = box_of((near_dx_lo, near_dx_hi), (near_dy_lo, near_dy_hi));
    // Captured unless `--nocapture`, and the reason every message below
    // quotes a box rather than a fraction: a single number cannot tell a
    // too-small bob from one on the wrong axis.
    println!("bob probe at infinity: {far_box}\nbob probe at 3 blocks: {near_box}");

    // --- 3. The nod, in isolation, against vanilla's constant. -----------
    // `abs(cos(bd*PI - 0.2) * bob) * 5.0` degrees, peaking at `bob * 5`.
    // A rotation of `t` about eye-space +X lifts an on-axis point at
    // infinity to `ndc_y = tan(t) / tan(fov_y / 2)`, i.e. *up* the screen.
    let nod_peak_deg = BOB_CEILING * 5.0;
    let nod_peak_px =
        VIEW_H * 0.5 * nod_peak_deg.to_radians().tan() / 35.0f32.to_radians().tan();
    assert!(
        (far_dy_lo + nod_peak_px).abs() < nod_peak_px * 0.015,
        "the nod must reach the projection at vanilla's full 0.5 deg: expected \
         a peak of -{nod_peak_px:.3} px on a point at infinity, measured \
         {far_box}. Zero here is a nod-free bob, which the chest-bbox pixel \
         gate cannot tell from a correct one."
    );
    assert!(
        far_dy_hi < 0.02,
        "the nod is rectified (`abs`), so a point at infinity may only ever \
         move *up*; measured {far_box}"
    );
    assert!(
        far_dx_lo > -0.05 && far_dx_hi < 0.05,
        "the bob must not yaw: the sway is a pure translation and cannot move \
         a point at infinity, and the fold drops the roll rather than \
         smearing it onto yaw. Measured {far_box}"
    );

    // --- 4. The translation, on the near point, by axis and by shape. ----
    // Sway: `sin(bd*PI) * bob * 0.5`, so +/-0.05 blocks laterally. At 3
    // blocks that is `0.05/3` of an eye-space unit, and the horizontal half
    // angle is `tan(35 deg) * aspect`.
    let sway_px = VIEW_W * 0.5 * (BOB_CEILING * 0.5 / 3.0)
        / (35.0f32.to_radians().tan() * ASPECT);
    assert!(
        near_dx_hi > sway_px * 0.9 && near_dx_lo < -sway_px * 0.9,
        "the sway is a full sine and must swing the near point *both* ways by \
         about {sway_px:.3} px; measured {near_box}"
    );
    // Dip: `-abs(cos(bd*PI) * bob)`, so the eye drops up to 0.1 blocks and a
    // point 3 blocks ahead rises 0.1/3 of a unit *in eye space*, i.e. moves
    // **down** the screen. Rectified, so it is one-way, and it is opposed by
    // the nod near the phase where the dip vanishes — hence a floor on the
    // downward peak rather than a sign assertion.
    let dip_px = VIEW_H * 0.5 * (BOB_CEILING / 3.0) / 35.0f32.to_radians().tan();
    assert!(
        near_dy_hi > (dip_px - nod_peak_px) * 0.9,
        "the dip must drop the eye a full 0.1 blocks, pushing a point 3 blocks \
         ahead down by about {:.3} px net of the nod; measured {near_box}",
        dip_px - nod_peak_px
    );
}

#[test]
