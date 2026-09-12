use super::*;
use glam::{Mat4, Vec3};
use lodestone_model::{CampfireSlot, ShelfSlot};

use crate::banner_pattern::{DyeColor, StoredPatternLayer};
use crate::camera::Frustum;
    use crate::camera::Camera;

    fn set() -> BlockEntityModelSet {
        BlockEntityModelSet::load()
    }

    /// The enchanting table's openness is the *live* form of the expression the
    /// lectern's is the frozen case of, and the two must not be confused.
    ///
    /// Predicted from outside constants: at `open == 1.2` (the lectern's fourth
    /// argument) and `time == 0` this must land on exactly `1.5`, the lectern's
    /// own value — that is the arithmetic identity the two share. Away from
    /// `time == 0` it must **leave** `1.5`, and by a predicted amount: the term is
    /// `sin(time * 0.02) * 0.1 * open`, so at `time = PI/2 / 0.02` the `sin` is
    /// exactly `1` and the result is `1.35 * 1.2 == 1.62`.
    ///
    /// A sign-only "it changes with time" assertion is satisfied by any live term
    /// at all, including one that varies `10×` too fast, which is what dropping
    /// the `0.02` produces.
    #[test]
    fn the_book_openness_breathes_around_the_lecterns_frozen_value() {
        let at_zero = enchanting_table_book_openness(0.0, 1.2);
        assert!(
            (at_zero - LECTERN_BOOK_OPENNESS).abs() < 1e-5,
            "time 0 gives {at_zero}, expected the lectern's {LECTERN_BOOK_OPENNESS}"
        );
        let peak_time = std::f32::consts::FRAC_PI_2 / 0.02;
        let at_peak = enchanting_table_book_openness(peak_time, 1.2);
        assert!(
            (at_peak - 1.62).abs() < 1e-4,
            "peak gives {at_peak}, expected 1.62 (1.35 * 1.2)"
        );
        // The trough is the mirror image, `1.15 * 1.2`.
        let trough = enchanting_table_book_openness(-peak_time, 1.2);
        assert!(
            (trough - 1.38).abs() < 1e-4,
            "trough gives {trough}, expected 1.38 (1.15 * 1.2)"
        );
    }

    /// The two page-flip phases are **half a period apart**, so the pages turn
    /// alternately, and both are clamped into `0..1`.
    ///
    /// The predicate that matters is "how far apart", not "are they different":
    /// two phases that differ by any amount at all satisfy the weak version, and
    /// reading both offsets as `0.25` (an easy transcription slip, since the
    /// expressions are otherwise identical) leaves them differing by zero — but
    /// so does *any* pair of offsets during the clamped stretches. So this asserts
    /// the unclamped interior: at `flip == 0` the raw values are
    /// `frac(0.25) * 1.6 - 0.3 == 0.1` and `frac(0.75) * 1.6 - 0.3 == 0.9`, i.e.
    /// exactly the lectern's own constant pair.
    #[test]
    fn the_two_page_flips_are_half_a_period_apart_and_clamped() {
        let (a, b) = enchanting_table_page_flips(0.0);
        assert!(
            (a - LECTERN_BOOK_PAGE_FLIP.0).abs() < 1e-5
                && (b - LECTERN_BOOK_PAGE_FLIP.1).abs() < 1e-5,
            "flip 0 gives ({a}, {b}), expected the lectern's {LECTERN_BOOK_PAGE_FLIP:?}"
        );
        // Half a period on: the two swap roles rather than both advancing.
        let (a, b) = enchanting_table_page_flips(0.5);
        assert!(
            (a - LECTERN_BOOK_PAGE_FLIP.1).abs() < 1e-5
                && (b - LECTERN_BOOK_PAGE_FLIP.0).abs() < 1e-5,
            "flip 0.5 gives ({a}, {b}), expected the pair swapped"
        );
        // The clamp fires, and it fires in both directions across one period —
        // the control for the clamp being load-bearing rather than decorative.
        let mut saw_low = false;
        let mut saw_high = false;
        for step in 0..200 {
            let flip = step as f32 / 100.0;
            let (a, b) = enchanting_table_page_flips(flip);
            for value in [a, b] {
                assert!(
                    (0.0..=1.0).contains(&value),
                    "flip {flip} produced {value}, outside 0..1"
                );
                saw_low |= value == 0.0;
                saw_high |= value == 1.0;
            }
        }
        assert!(
            saw_low && saw_high,
            "the clamp never fired across a full period, so it is untested here"
        );
    }

    /// An enchanting table's book is tilted `80°`, hovers, and turns about a
    /// *simulated* angle — three things that separate it from the lectern's book,
    /// which shares its every vertex.
    ///
    /// The tilt is asserted by where the book's local `+Y` ends up rather than by
    /// restating the constant: at `80°` about `Z` the local up axis leans
    /// `sin(80°) = 0.985` along world `-X` and keeps only `cos(80°) = 0.174` of
    /// its height. The lectern's `67.5°` would give `0.924`/`0.383`, so the two
    /// are separated by far more than any tolerance here.
    #[test]
    fn the_enchanting_table_book_is_tilted_eighty_degrees_and_hovers() {
        const POS: [i32; 3] = [3, 64, 7];
        let m = enchanting_table_book_placement_matrix(POS, 0.0, 0.0);
        let up = m.transform_vector3(Vec3::Y);
        assert!(
            (up.y - ENCHANTING_TABLE_BOOK_TILT_DEG.to_radians().cos()).abs() < 1e-4,
            "local up ends at {up:?}; a 67.5 degree tilt would keep 0.383 of its height"
        );
        // The hover: ±0.01 blocks about `0.75 + 0.1`, and its extremes are a
        // quarter period of `time * 0.1` apart.
        let base = Vec3::new(POS[0] as f32 + 0.5, POS[1] as f32 + 0.85, POS[2] as f32 + 0.5);
        let rest = m.transform_point3(Vec3::ZERO);
        assert!(
            rest.distance(base) < 1e-5,
            "rest origin {rest:?}, expected {base:?}"
        );
        let peak_time = std::f32::consts::FRAC_PI_2 / 0.1;
        let peak = enchanting_table_book_placement_matrix(POS, 0.0, peak_time)
            .transform_point3(Vec3::ZERO);
        assert!(
            (peak.y - (base.y + 0.01)).abs() < 1e-5,
            "hover peak at {}, expected {}",
            peak.y,
            base.y + 0.01
        );
    }

    /// The book turns about `y_rot` in **radians**, not degrees.
    ///
    /// Both hypotheses computed: a quarter turn is `PI/2` radians, and the same
    /// number read as degrees is `1.57°` — under two degrees, which on a book a
    /// few texels wide is visually indistinguishable from no rotation at all. So
    /// the assertion is that a `PI/2` input really does move the book's local `+Z`
    /// a full quarter turn, and that the degrees reading would leave it within
    /// `0.001` of where it started.
    ///
    /// Local `+Z` and **not** `+X`, and that is not arbitrary: the `80°` tilt
    /// about `Z` leaves local `Z` alone but stands local `X` almost vertical
    /// (`sin(80°) = 0.985` of it), and a Y rotation barely moves a near-vertical
    /// axis — measured, on the first run of this test, as `14°` for a genuine
    /// quarter turn. Picking the wrong probe axis here fails a correct
    /// implementation.
    #[test]
    fn the_book_yaw_is_radians_not_degrees() {
        const POS: [i32; 3] = [0, 0, 0];
        let rest = enchanting_table_book_placement_matrix(POS, 0.0, 0.0)
            .transform_vector3(Vec3::Z)
            .normalize();
        let quarter = enchanting_table_book_placement_matrix(POS, std::f32::consts::FRAC_PI_2, 0.0)
            .transform_vector3(Vec3::Z)
            .normalize();
        assert!(
            rest.dot(quarter).abs() < 1e-4,
            "PI/2 turned the book by {} degrees, not 90",
            rest.dot(quarter).acos().to_degrees()
        );
        let as_degrees =
            enchanting_table_book_placement_matrix(POS, std::f32::consts::FRAC_PI_2.to_radians(), 0.0)
                .transform_vector3(Vec3::Z)
                .normalize();
        assert!(
            rest.dot(as_degrees) > 0.999,
            "the degrees reading must be a near-no-op, and measured {}",
            rest.dot(as_degrees)
        );
    }

    /// The four cooking slots land in four **distinct** corners of the campfire's
    /// own block, clockwise seen from above, every one lifted onto its top face.
    ///
    /// Predicted from the pose stack, not read off the implementation: the
    /// pre-yaw offset is `Rx(90°) · (-0.3125, -0.3125, 0) = (-0.3125, 0, -0.3125)`,
    /// so a south-facing campfire's slot 0 sits at `(0.1875, 0.44921875, 0.1875)`
    /// and each further slot turns that a quarter turn about the block centre. A
    /// "four items somewhere on the campfire" assertion would accept all four
    /// stacked in one corner, which is what dropping the yaw term produces.
    #[test]
    fn the_four_campfire_slots_land_in_four_distinct_corners() {
        const POS: [i32; 3] = [10, 64, -3];
        let base = Vec3::new(POS[0] as f32, POS[1] as f32, POS[2] as f32);
        let expected = [
            Vec3::new(0.1875, CAMPFIRE_ITEM_LIFT, 0.1875),
            Vec3::new(0.8125, CAMPFIRE_ITEM_LIFT, 0.1875),
            Vec3::new(0.8125, CAMPFIRE_ITEM_LIFT, 0.8125),
            Vec3::new(0.1875, CAMPFIRE_ITEM_LIFT, 0.8125),
        ];
        for raw_slot in 0..CAMPFIRE_SLOTS {
            let slot = CampfireSlot::new(raw_slot as u8).expect("bounded campfire loop");
            let origin = campfire_item_matrix(POS, 0.0, slot).transform_point3(Vec3::ZERO);
            let want = base + expected[slot.index()];
            assert!(
                origin.distance(want) < 1e-5,
                "slot {raw_slot} pose origin {origin:?}, expected {want:?}"
            );
        }
    }

    /// Vanilla's own per-direction offset override, predicted from the real jar's
    /// arithmetic rather than restated: at `dust_progress = 3` (the maximum,
    /// vanilla's own completion-state accessor's own ceiling),
    /// `completionOffset = 3 / 10.0 * 0.75 = 0.225`, so an `EAST` hit pushes the
    /// item's `x` to `0.73 + 0.225 = 0.955` and leaves `y`/`z` at the base
    /// `0.0`/`0.5`.
    #[test]
    fn brushable_offset_moves_outward_along_the_hit_face() {
        use lodestone_assets::Direction;
        let offset = brushable_item_offset(Direction::East, 3);
        assert!(
            (offset.x - 0.955).abs() < 1e-5,
            "east offset.x = {}, expected 0.955",
            offset.x
        );
        assert!((offset.y - 0.0).abs() < 1e-5);
        assert!((offset.z - 0.5).abs() < 1e-5);

        let west = brushable_item_offset(Direction::West, 3);
        assert!(
            (west.x - 0.025).abs() < 1e-5,
            "west offset.x = {}, expected 0.025 (0.25 - 0.225)",
            west.x
        );
    }

    /// Every hit direction moves the item to a **distinct** point at the same
    /// `dust_progress`, and the outward distance grows monotonically with
    /// progress — the two properties vanilla's own per-direction offset table
    /// exists to give: which face, and how far the dig has revealed the item.
    #[test]
    fn brushable_offset_differs_per_direction_and_grows_with_progress() {
        use lodestone_assets::Direction;
        const DIRS: [Direction; 6] = [
            Direction::Down,
            Direction::Up,
            Direction::West,
            Direction::North,
            Direction::East,
            Direction::South,
        ];
        let mut at_zero = Vec::new();
        for d in DIRS {
            let o = brushable_item_offset(d, 0);
            assert!(
                at_zero.iter().all(|p: &Vec3| p.distance(o) > 1e-4),
                "direction {d:?} collided with an earlier direction at {o:?}"
            );
            at_zero.push(o);
        }
        for d in DIRS {
            let near = BRUSHABLE_ITEM_BASE_OFFSET.distance(brushable_item_offset(d, 1));
            let far = BRUSHABLE_ITEM_BASE_OFFSET.distance(brushable_item_offset(d, 3));
            assert!(
                far > near,
                "{d:?}: distance from base did not grow with dust_progress ({near} -> {far})"
            );
        }
    }

    /// The extra `Ry` term is `11°` on north/south/up/down and `101°` on
    /// east/west — `(east_west ? 90 : 0) + 11`, predicted rather than restated.
    /// Told apart via the local `+Z` axis after the fixed `75°` turn is undone
    /// algebraically: composing the matrix's own rotation out would just
    /// restate the code under test, so this instead checks the two east/west
    /// results agree with each other and disagree with the four others, which
    /// a dropped `east_west` branch (always `+11°`) cannot produce.
    #[test]
    fn brushable_matrix_turns_an_extra_ninety_degrees_on_the_horizontal_axis() {
        use lodestone_assets::Direction;
        const POS: [i32; 3] = [5, 70, 5];
        let east = brushable_item_matrix(POS, Direction::East, 2);
        let west = brushable_item_matrix(POS, Direction::West, 2);
        let north = brushable_item_matrix(POS, Direction::North, 2);
        let south = brushable_item_matrix(POS, Direction::South, 2);
        let east_x = east.transform_vector3(Vec3::X).normalize();
        let west_x = west.transform_vector3(Vec3::X).normalize();
        let north_x = north.transform_vector3(Vec3::X).normalize();
        let south_x = south.transform_vector3(Vec3::X).normalize();
        assert!(
            east_x.dot(west_x) > 0.999,
            "east/west should share the same +90 extra turn"
        );
        assert!(
            north_x.dot(south_x) > 0.999,
            "north/south should share the same +0 extra turn"
        );
        assert!(
            east_x.dot(north_x) < 0.999,
            "east/west and north/south must differ by the extra 90 degrees"
        );
    }

    /// `(slot - 1) * 0.3125`: slot 1 sits exactly on the shelf's own local
    /// x-axis centre (offset `0`), slot 0 sits `0.3125` in the local frame to
    /// one side and slot 2 the same distance to the other — predicted from
    /// the constant, not restated.
    #[test]
    fn shelf_slots_are_evenly_spaced_about_the_centre_slot() {
        let s0 = shelf_item_offset(ShelfSlot::new(0).expect("fixed shelf slot"), false);
        let s1 = shelf_item_offset(ShelfSlot::new(1).expect("fixed shelf slot"), false);
        let s2 = shelf_item_offset(ShelfSlot::new(2).expect("fixed shelf slot"), false);
        assert!((s1.x - 0.0).abs() < 1e-6, "slot 1 must sit on centre, got {}", s1.x);
        assert!(
            (s0.x - (-0.3125)).abs() < 1e-6,
            "slot 0 must sit 0.3125 left of centre, got {}",
            s0.x
        );
        assert!(
            (s2.x - 0.3125).abs() < 1e-6,
            "slot 2 must sit 0.3125 right of centre, got {}",
            s2.x
        );
        // `z` and the align-to-bottom `y` term are shared by every slot.
        for s in [s0, s1, s2] {
            assert!((s.z - (-0.25)).abs() < 1e-6);
            assert!((s.y - 0.0).abs() < 1e-6);
        }
    }

    /// `align_items_to_bottom` moves every slot's `y` offset to `-0.25` in
    /// the pre-scale local frame, and leaves `x`/`z` untouched.
    #[test]
    fn shelf_align_to_bottom_only_changes_the_y_offset() {
        for raw_slot in 0..SHELF_SLOTS {
            let slot = ShelfSlot::new(raw_slot as u8).expect("bounded shelf loop");
            let top = shelf_item_offset(slot, false);
            let bottom = shelf_item_offset(slot, true);
            assert!((bottom.x - top.x).abs() < 1e-6);
            assert!((bottom.z - top.z).abs() < 1e-6);
            assert!(
                (bottom.y - SHELF_ALIGN_BOTTOM_OFFSET).abs() < 1e-6,
                "aligned-to-bottom y should be {SHELF_ALIGN_BOTTOM_OFFSET}, got {}",
                bottom.y
            );
        }
    }

    /// `shelf_slot_matrix`'s rotation term is `Ry(-facing_yaw_deg)`, the same
    /// sign [`block_entity_placement_matrix`] uses for a chest — a south-facing
    /// shelf (`facing_yaw_deg = 0`) applies no rotation at all, so its slot
    /// centre sits directly on the block's own centre column.
    #[test]
    fn shelf_matrix_rotates_by_the_negated_facing_yaw() {
        const POS: [i32; 3] = [2, 64, 9];
        let south = shelf_slot_matrix(POS, 0.0, ShelfSlot::new(1).expect("fixed shelf slot"), false);
        let origin = south.transform_point3(Vec3::ZERO);
        // Slot 1's offset is `(0, 0, -0.25)` and the offset translate happens
        // *before* the `0.25x` scale in the pose stack, so it lands unscaled:
        // `pos + (0.5, 0.5, 0.5) + (0, 0, -0.25)`.
        let expected = Vec3::new(
            POS[0] as f32 + 0.5,
            POS[1] as f32 + 0.5,
            POS[2] as f32 + 0.5 - 0.25,
        );
        assert!(
            origin.distance(expected) < 1e-5,
            "south-facing slot-1 origin {origin:?}, expected {expected:?}"
        );
        // A 90-degree facing turns the local x-offset direction; south vs west
        // must therefore disagree, which a dropped rotation term cannot
        // produce.
        let west = shelf_slot_matrix(POS, 90.0, ShelfSlot::new(0).expect("fixed shelf slot"), false);
        let south0 = shelf_slot_matrix(POS, 0.0, ShelfSlot::new(0).expect("fixed shelf slot"), false);
        assert!(
            south0.transform_point3(Vec3::ZERO).distance(west.transform_point3(Vec3::ZERO)) > 1e-3,
            "a 90 degree facing change must move slot 0's world position"
        );
    }

    /// `Rz(180°) == scale(-1, -1, 1)`, algebraically — measured, not asserted
    /// from the trig identity alone, so the placement matrix's own
    /// determinant is checked against the value that identity predicts.
    /// `det(RotY) = det(RotZ(180°)) = 1`, so the whole placement — despite
    /// visually "flipping" a Y-down rig upright — is a proper rotation, the
    /// same `det == +1` invariant [`skull_placement_preserves_orientation`]
    /// holds for the entity flip's `scale(-1,-1,1)` form.
    #[test]
    fn copper_golem_statue_placement_preserves_orientation() {
        for yaw in [0.0_f32, 90.0, 180.0, 270.0] {
            let m = copper_golem_statue_placement_matrix([4, 70, 4], yaw);
            assert!(
                (m.determinant() - 1.0).abs() < 1e-4,
                "yaw {yaw}: det {}",
                m.determinant()
            );
        }
    }

    /// The rotation term is the **opposite** of `facing_yaw_deg`, not the
    /// facing itself — vanilla's own per-direction transformation map takes
    /// the facing direction's opposite's yaw, the one real surprise in this
    /// placement. A south-facing
    /// statue (`facing_yaw_deg = 0`, opposite = north = `180°`) must
    /// therefore differ from a north-facing one (`facing_yaw_deg = 180`,
    /// opposite = south = `0°`) — if the opposite term were dropped, south
    /// and north would swap results entirely, but *comparing north's real
    /// world direction against south's* is what actually pins the sign, not
    /// merely "the two differ".
    #[test]
    fn copper_golem_statue_rotates_by_the_opposite_of_its_facing() {
        const POS: [i32; 3] = [0, 64, 0];
        // Predicted by composing both matrix factors, not just the RotY term
        // alone (dropping the model's own Rz(180) from the prediction is an
        // easy mistake, and would silently swap the two expected answers
        // below): `M*X = RotY(-opposite_yaw) * (RotZ(180) * X)`, and
        // `RotZ(180) * X = -X` regardless of yaw.
        //
        // South-facing (`facing_yaw_deg = 0`): opposite = north = `180°`,
        // so `RotY(-180) * -X = +X` — local +X ends up unchanged.
        // North-facing (`facing_yaw_deg = 180`): opposite = south = `0°`,
        // so `RotY(0) * -X = -X` — local +X ends up flipped.
        // The two must therefore *disagree*, which a placement using
        // `facing_yaw_deg` directly (instead of its opposite) cannot
        // reproduce: that wrong hypothesis gives south `RotY(0)*-X = -X` and
        // north `RotY(-180)*-X = +X` — the same two answers, swapped.
        let south = copper_golem_statue_placement_matrix(POS, 0.0);
        let north = copper_golem_statue_placement_matrix(POS, 180.0);
        let south_x = south.transform_vector3(Vec3::X).normalize();
        let north_x = north.transform_vector3(Vec3::X).normalize();
        assert!(
            south_x.dot(Vec3::X) > 0.999,
            "south-facing (opposite=north=180deg) should leave local +X at world +X, got {south_x:?}"
        );
        assert!(
            north_x.dot(-Vec3::X) > 0.999,
            "north-facing (opposite=south=0deg) should point local +X at world -X, got {north_x:?}"
        );
    }

    /// `Rz(180°)` flips both the model's local X and Y axes (never Z) —
    /// exactly the `scale(-1, -1, 1)` every Y-down mob rig needs when placed
    /// as a block entity, told apart from a wrong single-axis flip (which
    /// would also satisfy `det == +1`, so that test alone cannot catch it).
    #[test]
    fn copper_golem_statue_flips_x_and_y_but_not_z() {
        // Cancel the rotation term by using yaw 180 (opposite = 0, no
        // rotation), isolating the model's own Rz(180) flip.
        let m = copper_golem_statue_placement_matrix([0, 0, 0], 180.0);
        let local_x = m.transform_vector3(Vec3::X);
        let local_y = m.transform_vector3(Vec3::Y);
        let local_z = m.transform_vector3(Vec3::Z);
        assert!(local_x.dot(Vec3::X) < -0.999, "local X must flip: {local_x:?}");
        assert!(local_y.dot(Vec3::Y) < -0.999, "local Y must flip: {local_y:?}");
        assert!(local_z.dot(Vec3::Z) > 0.999, "local Z must NOT flip: {local_z:?}");
    }

    /// `(slot + facing.get2DDataValue()) % 4`: turning the campfire a quarter turn
    /// moves slot 0 to where slot 1 was, so a campfire facing west puts its first
    /// item where a south-facing one puts its second.
    ///
    /// The control is built in: dropping the facing term makes every arm of this
    /// loop compare a point against **itself at slot 0**, i.e. it would require
    /// all four facings to agree, which they must not.
    #[test]
    fn the_facing_offsets_which_corner_each_slot_uses() {
        const POS: [i32; 3] = [0, 70, 0];
        let mut seen = Vec::new();
        for facing_2d in 0..CAMPFIRE_SLOTS {
            let turned = campfire_item_matrix(POS, facing_2d as f32 * 90.0, CampfireSlot::new(0).expect("fixed campfire slot"))
                .transform_point3(Vec3::ZERO);
            let offset_slot =
                campfire_item_matrix(POS, 0.0, CampfireSlot::new(facing_2d as u8).expect("bounded facing index")).transform_point3(Vec3::ZERO);
            assert!(
                turned.distance(offset_slot) < 1e-5,
                "facing {}: slot 0 at {turned:?} but slot {facing_2d} of a south \
                 campfire is at {offset_slot:?}",
                facing_2d as f32 * 90.0
            );
            seen.push(turned);
        }
        for (i, a) in seen.iter().enumerate() {
            for b in &seen[i + 1..] {
                assert!(a.distance(*b) > 0.5, "two facings share a corner: {a:?}");
            }
        }
    }

    /// Vanilla's own X-axis rotation of 90 degrees is what makes a food sprite lie *on* the
    /// campfire instead of standing up out of it, and a missing `Rx` leaves the
    /// item vertical while every corner assertion above still passes.
    ///
    /// Asserted as two independent facts about the basis, plus the scale, so a
    /// rotation about the wrong axis fails one of them: the sprite's normal
    /// (`+Z`) becomes vertical, and its width axis (`+X`) stays horizontal.
    #[test]
    fn a_cooking_item_lies_flat_at_three_eighths_scale() {
        let m = campfire_item_matrix([0, 0, 0], 0.0, CampfireSlot::new(0).expect("fixed campfire slot"));
        let normal = m.transform_vector3(Vec3::Z);
        let across = m.transform_vector3(Vec3::X);
        assert!(
            normal.normalize().y.abs() > 0.999,
            "sprite normal {normal:?} is not vertical — the item is standing up"
        );
        assert!(
            across.normalize().y.abs() < 1e-5,
            "width axis {across:?} is not horizontal"
        );
        assert!(
            (across.length() - CAMPFIRE_ITEM_SCALE).abs() < 1e-6,
            "scale is {}, expected {CAMPFIRE_ITEM_SCALE}",
            across.length()
        );
    }

    #[test]
    fn every_ported_model_bakes_with_geometry_and_parts() {
        let set = set();
        assert_eq!(
            set.len(),
            28,
            "3 chest layers + 2 skull canvases + dragon head + piglin head + bell \
             + 4 banner parts (standing and wall, body and flag) + shield + \
             shulker box + book + decorated pot (base plus 4 side models) + 4 \
             conduit layers (eye, wind, shell, cage) + 4 copper golem statue poses"
        );
        for (name, mesh) in set.iter() {
            assert!(mesh.quad_count() > 0, "{name} baked no quads");
            assert_eq!(mesh.parts.len(), mesh.part_names.len());
            assert_eq!(mesh.parts.len(), mesh.part_rest.len());
        }
        for name in [CHEST_SINGLE, CHEST_LEFT, CHEST_RIGHT] {
            let mesh = set.get(name).unwrap();
            assert!(mesh.index_of("lid").is_some(), "{name} has no lid part");
            assert!(mesh.index_of("lock").is_some(), "{name} has no lock part");
            assert!(mesh.index_of("bottom").is_some(), "{name} has no bottom");
        }
        for name in [SKULL_MOB, SKULL_HUMANOID] {
            let mesh = set.get(name).unwrap();
            assert!(mesh.index_of("head").is_some(), "{name} has no head part");
        }
        let bell = set.get(BELL).unwrap();
        assert!(bell.index_of("bell_body").is_some(), "bell has no bell_body part");
        assert!(bell.index_of("bell_base").is_some(), "bell has no bell_base part");
        let banner_body = set.get(BANNER_BODY).unwrap();
        assert!(banner_body.index_of("pole").is_some(), "banner body has no pole part");
        assert!(banner_body.index_of("bar").is_some(), "banner body has no bar part");
        let banner_flag = set.get(BANNER_FLAG).unwrap();
        assert!(banner_flag.index_of("flag").is_some(), "banner flag has no flag part");
    }

    /// The rest AABB must land in `0..1` on Y for the chest layers — the
    /// assertion an entity-space (Y-flipped, `−1.501`) placement fails.
    /// Measured through the same `part_transforms` the draw uses, not from
    /// restated texel extents. Skull is deliberately excluded: it *is*
    /// authored entity-space (Y-down, see `skull_head_part`'s doc), so its
    /// rest bounds dip below zero on purpose — that is asserted by
    /// `skull_head_box_extends_below_its_pivot_like_a_mob_head` in the asset
    /// crate, not this one.
    #[test]
    fn rest_bounds_sit_inside_the_block_above_the_floor() {
        let set = set();
        for name in [CHEST_SINGLE, CHEST_LEFT, CHEST_RIGHT] {
            let mesh = set.get(name).unwrap();
            assert!(
                mesh.local_min.y >= -1e-5,
                "{name} dips below the floor: {}",
                mesh.local_min.y
            );
            assert!(
                (mesh.local_max.y - 14.0 / 16.0).abs() < 1e-4,
                "{name} closed lid tops at {} not 14/16",
                mesh.local_max.y
            );
        }
    }

    /// A chest's placement is a pure rigid motion: `det == +1` for every facing,
    /// so it cannot reverse a quad's winding. Measured, not asserted from
    /// "rotations have positive determinant".
    #[test]
    fn placement_preserves_orientation() {
        for name in ["south", "west", "north", "east"] {
            let yaw = horizontal_facing_yaw(name).expect(name);
            let m = block_entity_placement_matrix([3, 64, -7], yaw);
            assert!(
                (m.determinant() - 1.0).abs() < 1e-5,
                "{name}: det {}",
                m.determinant()
            );
        }
    }

    /// The concrete difference from the entity path, measured against the real
    /// `entity_model_matrix` rather than described. A block entity is neither
    /// flipped nor lifted.
    #[test]
    fn placement_does_not_flip_or_lift() {
        let pos = [0, 0, 0];
        let block = block_entity_placement_matrix(pos, 0.0);
        // The block-space origin stays at the block's own corner.
        let origin = block.transform_point3(Vec3::ZERO);
        assert!(origin.abs_diff_eq(Vec3::ZERO, 1e-5), "origin {origin}");
        // +Y stays +Y.
        let up = block.transform_vector3(Vec3::Y);
        assert!(up.abs_diff_eq(Vec3::Y, 1e-5), "up {up}");

        // The entity matrix, by contrast, flips Y and drops 1.501.
        let entity = crate::entity::entity_model_matrix(Vec3::ZERO, 0.0, 1.0);
        let entity_up = entity.transform_vector3(Vec3::Y);
        assert!(
            entity_up.y < 0.0,
            "the entity path is supposed to flip Y; if this fails the two \
             placements have converged and this test no longer measures anything"
        );
        // **`+1.501`, not `−1.501`** — measured, having first been written the
        // other way round. `entity_model_matrix` is
        // `translate(feet) · rotY · scale(−s,−s,s) · translate(0, −1.501, 0)`,
        // and the flip is applied *after* the lift, so the negative translate
        // comes out positive in world space. Reading the lift's sign off the
        // matrix expression left-to-right gets this backwards, which is the same
        // shape of error `CLAUDE.md` records for the depth-bias record.
        let entity_origin = entity.transform_point3(Vec3::ZERO);
        assert!(
            (entity_origin.y - crate::entity::MODEL_FEET_OFFSET).abs() < 1e-4,
            "entity origin y {}",
            entity_origin.y
        );
    }

    /// South is `0`, and reading the yaw off `Direction`'s *declaration* order
    /// instead (down/up/north/south/west/east) is a quarter-turn error on every
    /// chest in the world.
    #[test]
    fn facing_yaw_follows_direction_to_y_rot_not_declaration_order() {
        assert_eq!(horizontal_facing_yaw("south"), Some(0.0));
        assert_eq!(horizontal_facing_yaw("west"), Some(90.0));
        assert_eq!(horizontal_facing_yaw("north"), Some(180.0));
        assert_eq!(horizontal_facing_yaw("east"), Some(270.0));
        assert_eq!(horizontal_facing_yaw("up"), None);
        assert_eq!(horizontal_facing_yaw(""), None);
    }

    /// A north-facing chest's lock (which sticks out of the *front* at z ≈ 1 in
    /// model space) must end up on the low-Z side of the block. This is the
    /// check that a `+yaw` instead of `-yaw` would fail while every determinant
    /// and bounds test stayed green.
    #[test]
    fn facing_rotates_the_front_of_the_chest_to_the_named_side() {
        let set = set();
        let front_z = |facing: &str| -> Vec3 {
            let spawn = ChestSpawn {
                facing_yaw_deg: horizontal_facing_yaw(facing).unwrap(),
                ..ChestSpawn::at([0, 0, 0])
            };
            let inst = set.resolve_chest(&spawn).expect("resolve");
            let lock = set.get(inst.model).unwrap().index_of("lock").unwrap();
            // The lock's own pivot is at model (0, 9, 1); the latch box sits at
            // z 14..15 texels beyond it, i.e. the chest's front face.
            inst.part_transforms[lock].transform_point3(Vec3::new(0.5, 0.0, 15.0 / 16.0))
        };
        let south = front_z("south");
        let north = front_z("north");
        // Vanilla's south-facing chest has its latch on the +Z face.
        assert!(south.z > 0.9, "south latch z {}", south.z);
        assert!(north.z < 0.1, "north latch z {}", north.z);
        let west = front_z("west");
        let east = front_z("east");
        assert!(west.x < 0.1, "west latch x {}", west.x);
        assert!(east.x > 0.9, "east latch x {}", east.x);
    }

    /// The two easings, at both endpoints and the midpoint. `0.5 → 0.875` is the
    /// value that distinguishes vanilla's cubic ease-out from a linear ramp;
    /// the endpoints alone cannot.
    #[test]
    fn lid_easing_matches_vanillas_cubic_ease_out() {
        assert!((chest_lid_openness(0.0) - 0.0).abs() < 1e-6);
        assert!((chest_lid_openness(1.0) - 1.0).abs() < 1e-6);
        let mid = chest_lid_openness(0.5);
        assert!((mid - 0.875).abs() < 1e-6, "mid {mid}");
        assert!(mid > 0.5, "the ease must run ahead of linear");
        // Out-of-range input clamps rather than exploding into a lid that spins.
        assert!((chest_lid_openness(-1.0) - 0.0).abs() < 1e-6);
        assert!((chest_lid_openness(4.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn lid_angle_is_a_quarter_turn_backwards_when_fully_open() {
        assert!((chest_lid_x_rot(0.0) - 0.0).abs() < 1e-6);
        let full = chest_lid_x_rot(1.0);
        assert!(
            (full - -std::f32::consts::FRAC_PI_2).abs() < 1e-6,
            "full {full}"
        );
    }

    /// The animation has to *move geometry*, not merely produce a different
    /// number. A closed and an open chest must differ in the lid's part matrix
    /// and agree in the bottom's — the second half is what catches an override
    /// applied to the whole model instead of the named parts.
    #[test]
    fn opening_moves_the_lid_and_lock_and_leaves_the_bottom_alone() {
        let set = set();
        let closed = set.resolve_chest(&ChestSpawn::at([0, 0, 0])).unwrap();
        let open = set
            .resolve_chest(&ChestSpawn {
                openness: 1.0,
                ..ChestSpawn::at([0, 0, 0])
            })
            .unwrap();
        let mesh = set.get(closed.model).unwrap();
        let lid = mesh.index_of("lid").unwrap();
        let lock = mesh.index_of("lock").unwrap();
        let bottom = mesh.index_of("bottom").unwrap();
        assert_ne!(closed.part_transforms[lid], open.part_transforms[lid]);
        assert_ne!(closed.part_transforms[lock], open.part_transforms[lock]);
        assert_eq!(closed.part_transforms[bottom], open.part_transforms[bottom]);

        // And it moves the right way: a fully open lid's far edge rises above
        // the closed chest's own top, rather than sinking into the box.
        let far_edge = Vec3::new(0.5, 0.0, 14.0 / 16.0);
        let closed_y = closed.part_transforms[lid].transform_point3(far_edge).y;
        let open_y = open.part_transforms[lid].transform_point3(far_edge).y;
        assert!(
            open_y > closed_y + 0.3,
            "an open lid must rise: closed {closed_y}, open {open_y}"
        );
    }

    #[test]
    fn material_resolution_covers_every_chest_block_and_nothing_else() {
        assert_eq!(
            ChestMaterial::from_block_path("chest"),
            Some(ChestMaterial::Regular)
        );
        assert_eq!(
            ChestMaterial::from_block_path("trapped_chest"),
            Some(ChestMaterial::Trapped)
        );
        assert_eq!(
            ChestMaterial::from_block_path("ender_chest"),
            Some(ChestMaterial::Ender)
        );
        assert_eq!(
            ChestMaterial::from_block_path("oxidized_copper_chest"),
            Some(ChestMaterial::CopperOxidized)
        );
        assert_eq!(ChestMaterial::from_block_path("barrel"), None);
        assert_eq!(ChestMaterial::from_block_path("chest_boat"), None);
    }

    /// Vanilla's own chest-material accessor checks copper and ender *before* the
    /// seasonal flag, so December must not repaint them.
    #[test]
    fn christmas_overrides_only_plain_and_trapped_chests() {
        assert_eq!(
            chest_material_with_season(ChestMaterial::Regular, true),
            ChestMaterial::Christmas
        );
        assert_eq!(
            chest_material_with_season(ChestMaterial::Trapped, true),
            ChestMaterial::Christmas
        );
        assert_eq!(
            chest_material_with_season(ChestMaterial::Ender, true),
            ChestMaterial::Ender
        );
        assert_eq!(
            chest_material_with_season(ChestMaterial::CopperWeathered, true),
            ChestMaterial::CopperWeathered
        );
        assert_eq!(
            chest_material_with_season(ChestMaterial::Regular, false),
            ChestMaterial::Regular
        );
    }

    /// Ender has one sheet for all three halves; every other material has three
    /// distinct ones. A uniform `_left`/`_right` suffix rule would name
    /// `ender_left.png`, which does not exist in the jar.
    #[test]
    fn ender_uses_one_sheet_and_others_use_three() {
        for half in [ChestHalf::Single, ChestHalf::Left, ChestHalf::Right] {
            assert_eq!(
                chest_texture_stem(ChestMaterial::Ender, half),
                "entity/chest/ender"
            );
        }
        for material in CHEST_MATERIALS {
            if *material == ChestMaterial::Ender {
                continue;
            }
            let stems: Vec<&str> = [ChestHalf::Single, ChestHalf::Left, ChestHalf::Right]
                .into_iter()
                .map(|h| chest_texture_stem(*material, h))
                .collect();
            assert_eq!(
                stems.len(),
                stems.iter().collect::<std::collections::BTreeSet<_>>().len(),
                "{material:?} reuses a sheet across halves: {stems:?}"
            );
        }
    }

    /// The preload list is derived from the same match the renderer asks
    /// through, so it cannot go stale: 7 materials × 3 halves + 1 ender = 22.
    #[test]
    fn every_stem_the_renderer_can_ask_for_is_in_the_preload_list() {
        let stems = chest_texture_stems();
        assert_eq!(stems.len(), 22, "{stems:?}");
        for material in CHEST_MATERIALS {
            for half in [ChestHalf::Single, ChestHalf::Left, ChestHalf::Right] {
                let stem = chest_texture_stem(*material, half);
                assert!(stems.contains(&stem), "{stem} missing from the preload list");
            }
        }
    }

    /// A camera 4 blocks back on `-Z` looking down `+Z` (yaw `0`) at the origin
    /// block — chests at `[0,0,0]`/`[1,0,0]` are in view, one 400 blocks behind
    /// is not.
    fn looking_at_origin() -> Camera {
        Camera {
            position: Vec3::new(0.5, 0.5, -4.0),
            yaw: 0.0,
            pitch: 0.0,
            ..Camera::default()
        }
    }

    #[test]
    fn planning_batches_by_model_and_texture_and_culls_what_is_behind() {
        let set = set();
        let front = set.resolve_chest(&ChestSpawn::at([0, 0, 0])).unwrap();
        let trapped_same_mesh = set
            .resolve_chest(&ChestSpawn {
                material: ChestMaterial::Trapped,
                ..ChestSpawn::at([1, 0, 0])
            })
            .unwrap();
        assert_eq!(
            front.model, trapped_same_mesh.model,
            "a trapped chest shares the single-chest mesh; the batch key must \
             therefore include the texture"
        );
        let behind = set.resolve_chest(&ChestSpawn::at([0, 0, -400])).unwrap();

        let cam = looking_at_origin();
        let frame = plan_block_entities(
            &[front, trapped_same_mesh, behind],
            &Frustum::from_view_projection(cam.view_projection()),
        );
        assert_eq!(frame.stats.total, 3);
        assert_eq!(frame.stats.drawn, 2, "{:?}", frame.stats);
        assert_eq!(frame.stats.culled_frustum, 1);
        assert_eq!(
            frame.batches.len(),
            2,
            "two textures over one mesh must be two batches"
        );
        for batch in &frame.batches {
            assert_eq!(batch.count(), 1);
            assert_eq!(batch.parts.len(), set.get(batch.model).unwrap().parts.len());
        }
    }

    /// Two chests sharing model *and* texture must land in one batch with two
    /// instances per part — the whole point of instancing.
    #[test]
    fn identical_chests_share_one_batch() {
        let set = set();
        let a = set.resolve_chest(&ChestSpawn::at([0, 0, 0])).unwrap();
        let b = set.resolve_chest(&ChestSpawn::at([1, 0, 0])).unwrap();
        let cam = looking_at_origin();
        let frame =
            plan_block_entities(&[a, b], &Frustum::from_view_projection(cam.view_projection()));
        assert_eq!(frame.batches.len(), 1);
        assert_eq!(frame.batches[0].count(), 2);
        for part in &frame.batches[0].parts {
            assert_eq!(part.len(), 2);
        }
    }

    #[test]
    fn player_head_skin_urls_are_part_of_the_block_entity_batch_key() {
        let set = set();
        let first = std::sync::Arc::<str>::from("https://textures.minecraft.net/texture/first");
        let second = std::sync::Arc::<str>::from("https://textures.minecraft.net/texture/second");
        let a = set
            .resolve_skull(&SkullSpawn {
                skull_type: SkullType::Player,
                texture: BlockEntityTexture::PlayerSkin(first.clone()),
                ..SkullSpawn::at([0, 0, 0])
            })
            .expect("player skull must resolve");
        let same_url = set
            .resolve_skull(&SkullSpawn {
                skull_type: SkullType::Player,
                texture: BlockEntityTexture::PlayerSkin(first),
                ..SkullSpawn::at([1, 0, 0])
            })
            .expect("player skull must resolve");
        let other_url = set
            .resolve_skull(&SkullSpawn {
                skull_type: SkullType::Player,
                texture: BlockEntityTexture::PlayerSkin(second),
                ..SkullSpawn::at([2, 0, 0])
            })
            .expect("player skull must resolve");
        let frame = plan_block_entities(
            &[a, same_url, other_url],
            &Frustum::from_view_projection(looking_at_origin().view_projection()),
        );
        assert_eq!(frame.batches.len(), 2, "distinct URLs must not share a batch");
        let counts: Vec<_> = frame.batches.iter().map(BlockEntityBatch::count).collect();
        assert!(counts.contains(&2), "repeated URLs must share one batch: {counts:?}");
        assert!(counts.contains(&1), "the other URL needs its own batch: {counts:?}");
    }

    #[test]
    fn light_reaches_the_batch_unchanged() {
        let set = set();
        let dark = set
            .resolve_chest(&ChestSpawn {
                light: 0,
                ..ChestSpawn::at([0, 0, 0])
            })
            .unwrap();
        assert_eq!(dark.light, 0);
        let cam = looking_at_origin();
        let frame = plan_block_entities(&[dark], &Frustum::from_view_projection(cam.view_projection()));
        assert_eq!(frame.batches[0].lights, vec![0]);
    }

    #[test]
    fn an_unknown_half_degrades_to_a_whole_chest() {
        assert_eq!(ChestHalf::parse("single"), ChestHalf::Single);
        assert_eq!(ChestHalf::parse("left"), ChestHalf::Left);
        assert_eq!(ChestHalf::parse("right"), ChestHalf::Right);
        assert_eq!(ChestHalf::parse("sideways"), ChestHalf::Single);
    }

    // --- skull/head ---------------------------------------------------

    #[test]
    fn skull_type_from_path_covers_all_seven_vanilla_types_and_declines_the_rest() {
        assert_eq!(
            SkullType::from_block_path("skeleton_skull"),
            Some(SkullType::Skeleton)
        );
        assert_eq!(
            SkullType::from_block_path("skeleton_wall_skull"),
            Some(SkullType::Skeleton)
        );
        assert_eq!(
            SkullType::from_block_path("wither_skeleton_skull"),
            Some(SkullType::WitherSkeleton)
        );
        assert_eq!(
            SkullType::from_block_path("wither_skeleton_wall_skull"),
            Some(SkullType::WitherSkeleton)
        );
        assert_eq!(
            SkullType::from_block_path("zombie_head"),
            Some(SkullType::Zombie)
        );
        assert_eq!(
            SkullType::from_block_path("creeper_wall_head"),
            Some(SkullType::Creeper)
        );
        assert_eq!(
            SkullType::from_block_path("player_head"),
            Some(SkullType::Player)
        );
        assert_eq!(
            SkullType::from_block_path("dragon_head"),
            Some(SkullType::Dragon)
        );
        assert_eq!(
            SkullType::from_block_path("dragon_wall_head"),
            Some(SkullType::Dragon)
        );
        assert_eq!(
            SkullType::from_block_path("piglin_head"),
            Some(SkullType::Piglin)
        );
        assert_eq!(
            SkullType::from_block_path("piglin_wall_head"),
            Some(SkullType::Piglin)
        );
        // Not a skull at all.
        assert_eq!(SkullType::from_block_path("chest"), None);
        // Every vanilla type is reachable, and each reaches a *distinct*
        // model/sheet pair — the check that would have caught pointing the
        // dragon at the shared 8x8x8 box.
        assert_eq!(SKULL_TYPES.len(), 7);
        let models: std::collections::BTreeSet<_> =
            SKULL_TYPES.iter().map(|t| t.model()).collect();
        assert_eq!(models.len(), 4, "{models:?}");
        assert!(models.contains(SKULL_DRAGON) && models.contains(SKULL_PIGLIN));
    }

    #[test]
    fn every_skull_stem_is_in_the_preload_list() {
        let stems = skull_texture_stems();
        assert_eq!(stems.len(), 7, "{stems:?}");
        for t in SKULL_TYPES {
            assert!(
                stems.contains(&skull_texture_stem(*t)),
                "{t:?} missing from the preload list"
            );
        }
        // Distinct sheets — a copy-paste in `skull_texture_stem` collapsing
        // two types onto one file would still pass a naive coverage check.
        let unique: std::collections::BTreeSet<_> = stems.iter().collect();
        assert_eq!(unique.len(), stems.len(), "{stems:?}");
    }

    #[test]
    fn every_ported_skull_type_bakes_and_resolves() {
        let set = set();
        for t in SKULL_TYPES {
            let spawn = SkullSpawn {
                skull_type: *t,
                texture: BlockEntityTexture::Static(skull_texture_stem(*t)),
                ..SkullSpawn::at([0, 0, 0])
            };
            let inst = set
                .resolve_skull(&spawn)
                .unwrap_or_else(|| panic!("{t:?} did not resolve"));
            assert!(!inst.part_transforms.is_empty(), "{t:?}");
            assert_eq!(inst.texture, skull_texture_stem(*t));
        }
    }

    /// The dragon's jaw and the piglin's ears are **assigned** by
    /// vanilla's own animation update, not added to their authored rest pose, and at rest the
    /// assigned value differs from the authored one in both cases. Predicting
    /// both hypotheses is the point: reading the mesh's own rest pose gives
    /// `0.0` for the jaw and `±PI/6` for the ears, and both are plausible
    /// enough to survive a look at the screen.
    #[test]
    fn dragon_jaw_and_piglin_ears_rest_away_from_their_authored_pose() {
        let jaw = dragon_head_jaw_x_rot(SKULL_RESTING_ANIMATION_POS);
        assert!((jaw - 0.2).abs() < 1e-6, "jaw at rest is {jaw}, want 0.2");
        assert!(
            jaw.abs() > 1e-3,
            "the rest-pose hypothesis (a clamped-shut 0.0 jaw) must not also satisfy this"
        );

        let (left, right) = piglin_head_ear_z_rots(SKULL_RESTING_ANIMATION_POS);
        assert!((left + 0.7).abs() < 1e-6, "left ear at rest is {left}, want -0.7");
        assert!((right - 0.7).abs() < 1e-6, "right ear at rest is {right}, want 0.7");
        let authored = std::f32::consts::FRAC_PI_6;
        assert!(
            (right - authored).abs() > 0.15,
            "0.7 must be distinguishable from the authored +PI/6 ({authored})"
        );
    }

    /// The `1.2` asymmetry on the *left* ear only. It is invisible at rest —
    /// both ears evaluate to `±0.7` with or without it — so this gate has to
    /// pick a position where the two hypotheses separate. At `12.5` the left
    /// ear's own cosine is at `3*PI` (`-1`) while the shared one is at
    /// `2.5*PI` (`0`), which is the widest the two readings ever get:
    /// `-0.3` against `-0.5`.
    #[test]
    fn the_piglin_ear_asymmetry_is_only_visible_off_rest() {
        let rest = piglin_head_ear_z_rots(SKULL_RESTING_ANIMATION_POS);
        assert!(
            (rest.0 + rest.1).abs() < 1e-6,
            "at rest the two ears are exact mirrors, so rest cannot discriminate"
        );

        let (left, right) = piglin_head_ear_z_rots(12.5);
        assert!((left + 0.3).abs() < 1e-5, "left ear is {left}, want -0.3");
        assert!((right - 0.5).abs() < 1e-5, "right ear is {right}, want 0.5");
        // The wrong hypothesis: no `1.2`, so the left ear mirrors the right.
        let without_asymmetry = -right;
        assert!(
            (left - without_asymmetry).abs() > 0.15,
            "left {left} must not land on the no-asymmetry value {without_asymmetry}"
        );
    }

    /// `resolve_skull` must actually *apply* those two poses — the island
    /// check for the override block, since a correct formula nothing calls
    /// draws exactly like no formula at all. Compares each posed child's own
    /// world matrix against the same mesh resolved with no override.
    #[test]
    fn resolve_skull_poses_the_dragon_jaw_and_both_piglin_ears() {
        let set = set();
        // Collected across *both* subjects and every part, not asserted inside
        // the loop: an `assert!` per iteration stops at the dragon's jaw and
        // leaves both piglin ears an argument rather than an observation.
        // Under the neuter this reports 3 of 3.
        let mut unchanged: Vec<String> = Vec::new();
        for (skull_type, model, parts) in [
            (SkullType::Dragon, SKULL_DRAGON, &[DRAGON_HEAD_JAW_PART][..]),
            (SkullType::Piglin, SKULL_PIGLIN, &PIGLIN_HEAD_EAR_PARTS[..]),
        ] {
            let spawn = SkullSpawn {
                skull_type,
                texture: BlockEntityTexture::Static(skull_texture_stem(skull_type)),
                ..SkullSpawn::at([0, 0, 0])
            };
            let inst = set
                .resolve_skull(&spawn)
                .unwrap_or_else(|| panic!("{skull_type:?} did not resolve"));
            let mesh = set.get(model).expect("model in corpus");
            let unposed = mesh.part_transforms(inst.transform, &[]);
            for name in parts {
                let i = mesh.index_of(name).expect("posed part in mesh");
                if inst.part_transforms[i].abs_diff_eq(unposed[i], 1e-5) {
                    unchanged.push(format!("{skull_type:?}/{name}"));
                }
            }
        }
        assert!(
            unchanged.is_empty(),
            "kept their authored rest pose: {unchanged:?}"
        );
    }

    /// Ground and wall placement both preserve orientation (`det == +1`),
    /// same as the chest placements — measured, not assumed, because this is
    /// the one block-entity placement that *does* apply the entity-style
    /// `scale(-1, -1, 1)` flip and a sign mistake there would show up as a
    /// negative determinant, not merely "upside down".
    #[test]
    fn skull_placement_preserves_orientation() {
        for seg in [0u8, 4, 8, 12, 15] {
            let m = skull_ground_placement_matrix([1, 2, 3], seg);
            assert!(
                (m.determinant() - 1.0).abs() < 1e-4,
                "segment {seg}: det {}",
                m.determinant()
            );
        }
        for yaw in [0.0_f32, 90.0, 180.0, 270.0] {
            let m = skull_wall_placement_matrix([1, 2, 3], yaw);
            assert!(
                (m.determinant() - 1.0).abs() < 1e-4,
                "yaw {yaw}: det {}",
                m.determinant()
            );
        }
    }

    /// Unlike a chest, a floor skull genuinely flips Y — the mirror image of
    /// `placement_does_not_flip_or_lift`'s chest assertion. Getting this
    /// backwards would bury the head texture upside down while every bounds
    /// and determinant check stayed green.
    #[test]
    fn ground_skull_flips_y_like_an_entity_head() {
        let m = skull_ground_placement_matrix([0, 0, 0], 0);
        let up = m.transform_vector3(Vec3::Y);
        assert!(up.y < 0.0, "expected an entity-style flip, got {up}");
    }

    /// The rotation segment spins the head about the block's own centre
    /// pivot `(0.5, 0, 0.5)`, so that pivot must land in the same world point
    /// regardless of segment — only the *head*, not the block position,
    /// rotates.
    #[test]
    fn ground_segment_rotates_about_the_block_centre() {
        let pos = [2, 5, -3];
        let unrotated = skull_ground_placement_matrix(pos, 0);
        let rotated = skull_ground_placement_matrix(pos, 8); // 180 degrees
        let a = unrotated.transform_point3(Vec3::ZERO);
        let b = rotated.transform_point3(Vec3::ZERO);
        assert!(a.abs_diff_eq(b, 1e-4), "pivot moved: {a} vs {b}");
        let expected = Vec3::new(2.5, 5.0, -2.5);
        assert!(a.abs_diff_eq(expected, 1e-4), "{a}");
    }

    /// `dir.getStepX()/getStepZ()` recovered by trig against a hand-verified
    /// table (not derived from the function under test): south `(0, 1)`,
    /// west `(-1, 0)`, north `(0, -1)`, east `(1, 0)`. A sign slip here
    /// offsets a wall skull toward the wrong wall while it still renders a
    /// plausible skull shape.
    #[test]
    fn wall_offset_moves_toward_the_named_direction() {
        let cases = [
            ("south", 0.0_f32, 0.0_f32, 1.0_f32),
            ("west", 90.0, -1.0, 0.0),
            ("north", 180.0, 0.0, -1.0),
            ("east", 270.0, 1.0, 0.0),
        ];
        for (name, yaw, step_x, step_z) in cases {
            let m = skull_wall_placement_matrix([0, 0, 0], yaw);
            let origin = m.transform_point3(Vec3::ZERO);
            let expected = Vec3::new(0.5 - step_x * 0.25, 0.25, 0.5 - step_z * 0.25);
            assert!(
                origin.abs_diff_eq(expected, 1e-4),
                "{name}: got {origin}, expected {expected}"
            );
        }
    }

    /// A chest and a skull share neither model nor texture, so a frame
    /// holding both must batch them separately — the same coverage the chest
    /// `planning_batches_by_model_and_texture_and_culls_what_is_behind` test
    /// gives two chest materials, now across two entirely different corpora,
    /// proving [`plan_block_entities`]/[`BlockEntityInstance`] are generic
    /// over block-entity *family*, not just over chest variants.
    #[test]
    fn chests_and_skulls_batch_independently_in_one_frame() {
        let set = set();
        let chest = set.resolve_chest(&ChestSpawn::at([0, 0, 0])).unwrap();
        let skull = set.resolve_skull(&SkullSpawn::at([1, 0, 0])).unwrap();
        let cam = looking_at_origin();
        let frame = plan_block_entities(
            &[chest, skull],
            &Frustum::from_view_projection(cam.view_projection()),
        );
        assert_eq!(frame.stats.drawn, 2);
        assert_eq!(
            frame.batches.len(),
            2,
            "a chest and a skull must not share a batch"
        );
    }

    #[test]
    fn bell_stem_is_in_the_preload_list() {
        let stems = bell_texture_stems();
        assert_eq!(stems, vec![BELL_TEXTURE_STEM]);
        assert!(block_entity_texture_stems().contains(&BELL_TEXTURE_STEM));
    }

    /// Every stem [`shulker_texture_stem`] can return is preloaded, and the
    /// colour order is `DyeColor`'s **ordinal** order rather than the
    /// alphabetical one the texture directory suggests — reading it off the
    /// listing shifts every dyed box one sprite along, which draws a plausible
    /// wrong colour instead of nothing.
    #[test]
    fn every_shulker_stem_is_in_the_preload_list_in_dye_ordinal_order() {
        // `DyeColor`'s first four and last, from the enum's own declaration order
        // (vanilla's dye-colour registration), not from this table.
        assert_eq!(
            &SHULKER_COLOURS[..4],
            &["white", "orange", "magenta", "light_blue"]
        );
        assert_eq!(SHULKER_COLOURS[15], "black");
        assert_eq!(SHULKER_COLOURS.len(), 16);

        let preload = block_entity_texture_stems();
        assert!(preload.contains(&shulker_texture_stem(None)));
        for colour in SHULKER_COLOURS {
            let stem = shulker_texture_stem(Some(colour));
            assert_ne!(
                stem, SHULKER_DEFAULT_TEXTURE_STEM,
                "{colour} fell through to the undyed sheet"
            );
            assert!(preload.contains(&stem), "{stem} missing from the preload list");
        }
        // An unrecognised name degrades to the undyed sheet rather than being
        // dropped — a plain `shulker_box` has no colour segment at all.
        assert_eq!(
            shulker_texture_stem(Some("chartreuse")),
            SHULKER_DEFAULT_TEXTURE_STEM
        );
    }

    /// An upward-facing shulker box occupies its own block cell and nothing else.
    ///
    /// The expectation comes from geometry rather than from the matrix: the box is
    /// authored as a 16×20 texel stack (`base` 8 tall from y=−8, `lid` 12 tall from
    /// y=−16, both at pivot y=24), so once vanilla's `scale(1, -1, -1)` and
    /// `translate(0, -1, 0)` are folded in it must sit in `0..1` on every axis, at
    /// `0.9995` scale about the block centre. Reusing
    /// [`block_entity_placement_matrix`] instead (a floor pivot, no flip) puts the
    /// box a half-block low and upside down — which still looks like a box.
    #[test]
    fn an_upward_shulker_sits_inside_its_own_block() {
        let set = set();
        let box_at = set.resolve_shulker(&ShulkerSpawn::at([3, 5, -2])).unwrap();
        let lo = Vec3::from(box_at.aabb_min);
        let hi = Vec3::from(box_at.aabb_max);
        let cell = Vec3::new(3.0, 5.0, -2.0);
        assert!(
            lo.cmpge(cell - Vec3::splat(0.001)).all() && hi.cmple(cell + Vec3::splat(1.001)).all(),
            "an up-facing box escaped its own cell: {lo} .. {hi}"
        );
        // And it fills nearly all of it — the `0.9995` shrink, not a half-height
        // box. A `0.5`-tall result is the floor-pivot mistake above.
        let size = hi - lo;
        assert!(
            size.min_element() > 0.99,
            "the box is not block-sized: {size}"
        );
        assert_eq!(box_at.texture, SHULKER_DEFAULT_TEXTURE_STEM);
    }

    /// A closed box needs no part override at all; an open one moves only `lid`.
    /// This is what lets a shulker box share the existing `(model, texture)` batch
    /// key with no per-instance animation state.
    #[test]
    fn a_closed_shulker_is_the_rest_pose_and_an_open_one_moves_only_the_lid() {
        let set = set();
        let mesh = set.get(SHULKER_BOX).unwrap();
        let lid = mesh.index_of("lid").expect("the lid part is named `lid`");
        let base = mesh.index_of("base").expect("the base part is named `base`");

        let closed = set.resolve_shulker(&ShulkerSpawn::at([0, 0, 0])).unwrap();
        let rest = mesh.part_transforms(shulker_placement_matrix([0, 0, 0], ShulkerFacing::Up), &[]);
        assert_eq!(closed.part_transforms[lid], rest[lid]);

        let open = set
            .resolve_shulker(&ShulkerSpawn {
                progress: 1.0,
                ..ShulkerSpawn::at([0, 0, 0])
            })
            .unwrap();
        assert_ne!(open.part_transforms[lid], closed.part_transforms[lid]);
        assert_eq!(
            open.part_transforms[base], closed.part_transforms[base],
            "opening a box moved its base"
        );
        // `lid.setPos(0, 24 - progress * 0.5 * 16, 0)` and `yRot = 270 * progress`
        // — predicted from the jar, not read back out of the port.
        assert_eq!(shulker_lid_pose(0.0), (24.0, 0.0));
        let (y, y_rot) = shulker_lid_pose(1.0);
        assert_eq!(y, 16.0);
        assert!((y_rot - 270.0_f32.to_radians()).abs() < 1e-5, "{y_rot}");
    }

    /// The six facings are six distinct placements, and a down-facing box is the
    /// up-facing one turned over — the direction-to-rotation port.
    #[test]
    fn every_shulker_facing_is_a_distinct_placement() {
        let facings = [
            ShulkerFacing::Up,
            ShulkerFacing::Down,
            ShulkerFacing::North,
            ShulkerFacing::South,
            ShulkerFacing::West,
            ShulkerFacing::East,
        ];
        let mats: Vec<Mat4> = facings
            .iter()
            .map(|f| shulker_placement_matrix([0, 0, 0], *f))
            .collect();
        for i in 0..mats.len() {
            for j in (i + 1)..mats.len() {
                assert!(
                    !mats[i].abs_diff_eq(mats[j], 1e-5),
                    "{:?} and {:?} share a placement",
                    facings[i],
                    facings[j]
                );
            }
        }
        // Every facing still lands the box in its own cell, which is the property
        // an axis mix-up in `ShulkerFacing::rotation` breaks.
        let set = set();
        for facing in facings {
            let drawn = set
                .resolve_shulker(&ShulkerSpawn {
                    facing,
                    ..ShulkerSpawn::at([0, 0, 0])
                })
                .unwrap();
            let lo = Vec3::from(drawn.aabb_min);
            let hi = Vec3::from(drawn.aabb_max);
            assert!(
                lo.cmpge(Vec3::splat(-0.001)).all() && hi.cmple(Vec3::splat(1.001)).all(),
                "{facing:?} escaped its own cell: {lo} .. {hi}"
            );
        }
        assert_eq!(ShulkerFacing::from_name("up"), Some(ShulkerFacing::Up));
        assert_eq!(ShulkerFacing::from_name("sideways"), None);
        assert_eq!(ShulkerFacing::default(), ShulkerFacing::Up);
    }

    /// Vanilla's own bell animation update's exact formula, predicted independently of the
    /// port rather than by re-deriving its own arithmetic: choosing
    /// `ticks = pi^2 / 2` makes `sin(ticks / pi) == sin(pi/2) == 1` exactly,
    /// so the only remaining unknown is `base_rot = 1 / (4 + ticks/3)` and
    /// each direction's sign/axis — a magnitude check, not merely a sign
    /// flip (`CLAUDE.md`'s "predict the value, do not merely assert the
    /// sign" rule).
    #[test]
    fn bell_shake_angle_matches_the_exact_vanilla_formula() {
        assert_eq!(bell_shake_angle(None, 999.0), (0.0, 0.0), "no direction, no motion");
        assert_eq!(
            bell_shake_angle(Some(BellShakeDirection::North), 0.0),
            (0.0, 0.0),
            "sin(0) is zero at tick 0"
        );

        let ticks = std::f32::consts::PI * std::f32::consts::PI / 2.0;
        let expected = 1.0 / (4.0 + ticks / 3.0);

        let (x, z) = bell_shake_angle(Some(BellShakeDirection::North), ticks);
        assert!((x - -expected).abs() < 1e-4, "north x_rot {x}");
        assert_eq!(z, 0.0);

        let (x, z) = bell_shake_angle(Some(BellShakeDirection::South), ticks);
        assert!((x - expected).abs() < 1e-4, "south x_rot {x}");
        assert_eq!(z, 0.0);

        let (x, z) = bell_shake_angle(Some(BellShakeDirection::East), ticks);
        assert_eq!(x, 0.0);
        assert!((z - -expected).abs() < 1e-4, "east z_rot {z}");

        let (x, z) = bell_shake_angle(Some(BellShakeDirection::West), ticks);
        assert_eq!(x, 0.0);
        assert!((z - expected).abs() < 1e-4, "west z_rot {z}");
    }

    /// The rim (`bell_base`) has no override of its own — if shaking the body
    /// did not also move it, that would mean the parent/child nesting broke
    /// (see `bell_model`'s doc), not merely that the shake is small.
    #[test]
    fn shaking_the_body_moves_the_rim_too_because_it_is_a_child() {
        let set = set();
        let resting = set.resolve_bell(&BellSpawn::at([0, 0, 0])).unwrap();
        assert_eq!(resting.texture, BELL_TEXTURE_STEM);
        assert!(!resting.part_transforms.is_empty());

        let mesh = set.get(BELL).unwrap();
        let body = mesh.index_of("bell_body").unwrap();
        let base = mesh.index_of("bell_base").unwrap();

        let ticks = std::f32::consts::PI * std::f32::consts::PI / 2.0;
        let shaking = set
            .resolve_bell(&BellSpawn {
                shake: Some((BellShakeDirection::East, ticks)),
                ..BellSpawn::at([0, 0, 0])
            })
            .unwrap();

        assert_ne!(
            shaking.part_transforms[body], resting.part_transforms[body],
            "the body itself must move"
        );
        assert_ne!(
            shaking.part_transforms[base], resting.part_transforms[base],
            "the rim must move with its parent"
        );
    }

    #[test]
    fn bells_batch_independently_from_chests_and_skulls() {
        let set = set();
        let chest = set.resolve_chest(&ChestSpawn::at([0, 0, 0])).unwrap();
        let skull = set.resolve_skull(&SkullSpawn::at([1, 0, 0])).unwrap();
        let bell = set.resolve_bell(&BellSpawn::at([2, 0, 0])).unwrap();
        let cam = looking_at_origin();
        let frame = plan_block_entities(
            &[chest, skull, bell],
            &Frustum::from_view_projection(cam.view_projection()),
        );
        assert_eq!(frame.stats.drawn, 3);
        assert_eq!(
            frame.batches.len(),
            3,
            "a chest, a skull and a bell must not share a batch"
        );
    }

    // --- banner -----------------------------------------------------------

    /// `banner_ground_placement_matrix`'s scale flips **two** axes (Y and Z),
    /// like `skull_ground_placement_matrix`'s single-axis flip is paired with
    /// the rotation's own handedness — the product of an even number of sign
    /// flips preserves orientation. Measured, not assumed: this is the same
    /// "measure the determinant, don't assert it" discipline
    /// `placement_preserves_orientation` already holds the chest placement
    /// to, generalised to a matrix whose magnitude is `(2/3)^3`, not `1`, so
    /// the assertion is on the *sign* of the determinant, not its value.
    #[test]
    fn banner_ground_placement_preserves_orientation() {
        for segment in [0u8, 1, 4, 8, 12, 15] {
            let m = banner_ground_placement_matrix([3, 64, -7], segment);
            assert!(
                m.determinant() > 0.0,
                "segment {segment}: det {} should be positive (two axis flips cancel)",
                m.determinant()
            );
        }
    }

    /// The two flips are real, individually — not merely a determinant that
    /// happens to be positive by some other route. `+Y` and `+Z` must each
    /// reverse under the placement's linear part, the mirror image of
    /// `placement_does_not_flip_or_lift`'s "chest does not flip" assertion.
    #[test]
    fn banner_ground_placement_flips_y_and_z_but_not_x() {
        let m = banner_ground_placement_matrix([0, 0, 0], 0);
        let up = m.transform_vector3(Vec3::Y);
        let fwd = m.transform_vector3(Vec3::Z);
        let right = m.transform_vector3(Vec3::X);
        assert!(up.y < 0.0, "expected a Y flip, got {up}");
        assert!(fwd.z < 0.0, "expected a Z flip, got {fwd}");
        assert!(right.x > 0.0, "X must not flip, got {right}");
        // Magnitude is the real `2/3` scale, not `1` — skipping it would
        // render a banner 1.5x too large.
        assert!((up.length() - 2.0 / 3.0).abs() < 1e-5, "up length {}", up.length());
    }

    /// `banner_phase`'s exact `floorMod` formula: zero at the origin with no
    /// game time, wraps every 100 ticks, and a negative-leaning block
    /// coordinate sum still lands in `0..1` rather than going negative
    /// (Rust's `%` truncates toward zero and would fail this).
    #[test]
    fn banner_phase_matches_the_floor_mod_formula_and_wraps() {
        assert_eq!(banner_phase([0, 0, 0], 0, 0.0), 0.0);
        // sum = 7 (x=1), game_time 93 -> 100 -> floorMod 0.
        assert_eq!(banner_phase([1, 0, 0], 93, 0.0), 0.0);
        // Partial tick folds in additively, still divided by 100.
        let with_partial = banner_phase([0, 0, 0], 0, 0.5);
        assert!((with_partial - 0.005).abs() < 1e-6, "{with_partial}");
        // A coordinate sum that goes negative must still wrap into 0..100,
        // not produce a negative phase.
        let negative = banner_phase([-5, 0, 0], 0, 0.0);
        assert!((0.0..1.0).contains(&negative), "{negative}");
        // 7 * -5 = -35; floorMod(-35, 100) = 65 -> phase 0.65.
        assert!((negative - 0.65).abs() < 1e-6, "{negative}");
    }

    /// `banner_flag_x_rot`'s exact formula at three phases — a magnitude
    /// prediction, not merely "the sign changes" (`CLAUDE.md`'s "predict the
    /// value" rule). `cos` is exactly `1`, `0` and `-1` at these three
    /// phases, so every intermediate multiply is exact rather than
    /// approximate.
    #[test]
    fn banner_flag_x_rot_matches_the_exact_vanilla_formula() {
        let pi = std::f32::consts::PI;
        // phase 0: cos(0) = 1 -> (-0.0125 + 0.01) * pi = -0.0025 * pi.
        assert!((banner_flag_x_rot(0.0) - (-0.0025 * pi)).abs() < 1e-5);
        // phase 0.25: cos(pi/2) = 0 -> -0.0125 * pi exactly.
        assert!((banner_flag_x_rot(0.25) - (-0.0125 * pi)).abs() < 1e-5);
        // phase 0.5: cos(pi) = -1 -> (-0.0125 - 0.01) * pi = -0.0225 * pi.
        assert!((banner_flag_x_rot(0.5) - (-0.0225 * pi)).abs() < 1e-5);
    }

    /// The base mask is always present and first, even with zero stored
    /// patterns, and every stored pattern follows in its own order —
    /// `resolve_banner` reaching all the way to `banner_pattern_layers`'
    /// own contract (`no_patterns_still_draws_the_base_layer`/
    /// `pattern_order_is_preserved_exactly` in `banner_pattern.rs`), not
    /// re-deriving it.
    #[test]
    fn resolve_banner_produces_the_base_layer_plus_every_pattern_in_order() {
        let set = set();
        let patterns = vec![
            StoredPatternLayer {
                pattern_asset_id: "creeper".to_string(),
                color: DyeColor::Lime,
            },
            StoredPatternLayer {
                pattern_asset_id: "stripe_top".to_string(),
                color: DyeColor::Black,
            },
        ];
        let banner = set
            .resolve_banner(&BannerSpawn {
                base_color: DyeColor::Red,
                patterns,
                ..BannerSpawn::at([0, 0, 0])
            })
            .expect("banner_body and banner_flag must both be in the corpus");
        assert_eq!(banner.layers.len(), 3, "base + 2 patterns");
        assert_eq!(banner.layers[0].color, DyeColor::Red.gamma_rgb());
        assert_eq!(banner.layers[1].color, DyeColor::Lime.gamma_rgb());
        assert_eq!(banner.layers[2].color, DyeColor::Black.gamma_rgb());
        assert!(
            banner.layers[0].sprite.path().ends_with("banner/base"),
            "{:?}",
            banner.layers[0].sprite
        );
        assert!(
            banner.layers[1].sprite.path().ends_with("banner/creeper"),
            "{:?}",
            banner.layers[1].sprite
        );
    }

    /// Every layer reuses the *flag's* posed transform, never the body's —
    /// pattern masks paint over the cloth, not the pole/bar, and a wrong
    /// wiring here would have every mask draw at the pole's own (much
    /// smaller, differently pivoted) rect instead of the flag's.
    #[test]
    fn resolve_banner_layers_share_the_flag_transform_not_the_body() {
        let set = set();
        let banner = set.resolve_banner(&BannerSpawn::at([0, 0, 0])).unwrap();
        let flag_mesh = set.get(BANNER_FLAG).unwrap();
        let flag_index = flag_mesh.index_of("flag").unwrap();
        let expected = banner.flag.part_transforms[flag_index];
        for (i, layer) in banner.layers.iter().enumerate() {
            assert_eq!(layer.transform, expected, "layer {i} transform must equal the flag's");
        }
        assert_ne!(
            banner.layers[0].transform, banner.body.transform,
            "the layer transform must not be the bare placement (the body's)"
        );
    }

    /// The sway moves the flag's own transform, and every layer moves with
    /// it — the same "does it move geometry, not just produce a different
    /// number" standard `opening_moves_the_lid_and_lock_and_leaves_the_bottom_alone`
    /// holds the chest lid to, and `shaking_the_body_moves_the_rim_too_because_it_is_a_child`
    /// holds the bell rim to.
    #[test]
    fn resolve_banner_sway_moves_the_flag_and_every_layer_with_it() {
        let set = set();
        let resting = set
            .resolve_banner(&BannerSpawn {
                patterns: vec![StoredPatternLayer {
                    pattern_asset_id: "creeper".to_string(),
                    color: DyeColor::Lime,
                }],
                ..BannerSpawn::at([0, 0, 0])
            })
            .unwrap();
        let swaying = set
            .resolve_banner(&BannerSpawn {
                phase: 0.5,
                patterns: vec![StoredPatternLayer {
                    pattern_asset_id: "creeper".to_string(),
                    color: DyeColor::Lime,
                }],
                ..BannerSpawn::at([0, 0, 0])
            })
            .unwrap();
        assert_ne!(
            resting.flag.part_transforms, swaying.flag.part_transforms,
            "the flag itself must move"
        );
        assert_eq!(
            resting.body.part_transforms, swaying.body.part_transforms,
            "the pole/bar must not move — only the flag sways"
        );
        assert_ne!(
            resting.layers[0].transform, swaying.layers[0].transform,
            "every pattern layer must move with the flag"
        );
        assert_ne!(
            resting.layers[1].transform, swaying.layers[1].transform,
            "including the base layer"
        );
    }

    #[test]
    fn banner_texture_stem_is_shared_by_body_and_flag_and_in_the_preload_list() {
        let set = set();
        let banner = set.resolve_banner(&BannerSpawn::at([0, 0, 0])).unwrap();
        assert_eq!(banner.body.texture, BANNER_BASE_TEXTURE_STEM);
        assert_eq!(banner.flag.texture, BANNER_BASE_TEXTURE_STEM);
        assert_eq!(banner_texture_stems(), vec![BANNER_BASE_TEXTURE_STEM]);
        assert!(block_entity_texture_stems().contains(&BANNER_BASE_TEXTURE_STEM));
    }

    /// A banner's opaque body+flag batch independently from a chest — the
    /// same coverage `bells_batch_independently_from_chests_and_skulls`
    /// gives bells, now for the fourth family. The banner's own translucent
    /// `layers` are not part of `plan_block_entities` at all (by design —
    /// see `BannerInstances`' doc), so only `body`/`flag` go into this call.
    #[test]
    fn banner_body_and_flag_batch_independently_from_a_chest() {
        let set = set();
        let chest = set.resolve_chest(&ChestSpawn::at([2, 0, 0])).unwrap();
        let banner = set.resolve_banner(&BannerSpawn::at([0, 0, 0])).unwrap();
        let cam = looking_at_origin();
        let frame = plan_block_entities(
            &[chest, banner.body, banner.flag],
            &Frustum::from_view_projection(cam.view_projection()),
        );
        assert_eq!(frame.stats.drawn, 3);
        assert_eq!(
            frame.batches.len(),
            3,
            "chest, banner body and banner flag must all batch independently \
             (different model *and* different model between body/flag)"
        );
    }

    /// **A wall banner draws the pole-less rig, at the wall's own height.**
    ///
    /// Three assertions, each catching a different way this goes wrong while still
    /// drawing a recognisable banner:
    ///
    /// * the models are the *wall* pair, so the standing rig's 42-texel pole
    ///   cannot end up hanging in mid-air off a block face;
    /// * the wall body really has no `pole` part and the standing one does — an
    ///   `if (standing)` in `createBodyLayer` that was transcribed as
    ///   unconditional would give both a pole and pass any "two banner meshes
    ///   exist" check;
    /// * the two flags sit at **different heights**, which is the whole content of
    ///   the `standing ? -44 : -20.5` pose ternary. Their *cubes* are
    ///   byte-identical, so a copy that reused the standing pose produces a wall
    ///   banner buried two blocks into the floor and no assertion about geometry
    ///   would notice.
    #[test]
    fn a_wall_banner_uses_the_poleless_rig_and_hangs_at_its_own_height() {
        let set = set();
        let wall = set
            .resolve_banner(&BannerSpawn::on_wall([0, 0, 0], 180.0))
            .expect("both wall banner models must be in the corpus");
        assert_eq!(wall.body.model, BANNER_WALL_BODY);
        assert_eq!(wall.flag.model, BANNER_WALL_FLAG);
        assert_eq!(wall.body.texture, BANNER_BASE_TEXTURE_STEM, "one shared sheet");
        assert_eq!(wall.flag.texture, BANNER_BASE_TEXTURE_STEM);

        let standing_body = set.get(BANNER_BODY).unwrap();
        let wall_body = set.get(BANNER_WALL_BODY).unwrap();
        assert!(
            standing_body.index_of("pole").is_some(),
            "the standing body must have a pole"
        );
        assert!(
            wall_body.index_of("pole").is_none(),
            "createBodyLayer(false) adds no pole"
        );
        assert!(wall_body.index_of("bar").is_some());

        // The pose ternary, measured through the same `part_transforms` the draw
        // uses rather than by restating -44 and -20.5.
        let standing_flag = set.get(BANNER_FLAG).unwrap();
        let wall_flag = set.get(BANNER_WALL_FLAG).unwrap();
        let flag_y = |mesh: &BlockEntityMesh| {
            let i = mesh.index_of("flag").unwrap();
            mesh.part_transforms(Mat4::IDENTITY, &[])[i]
                .transform_point3(Vec3::ZERO)
                .y
        };
        let (standing_y, wall_y) = (flag_y(standing_flag), flag_y(wall_flag));
        assert!(
            (standing_y - -44.0 / 16.0).abs() < 1e-5,
            "standing flag pivot {standing_y}"
        );
        assert!(
            (wall_y - -20.5 / 16.0).abs() < 1e-5,
            "wall flag pivot {wall_y}"
        );
        assert!(
            wall_y > standing_y,
            "a wall banner hangs higher in model space than a standing one's \
             cloth: {wall_y} vs {standing_y}"
        );

        // The cubes are identical, which is exactly why the pose above is the
        // only thing separating them.
        assert_eq!(standing_flag.quad_count(), wall_flag.quad_count());
    }

    /// The two placements are one function with two angles — but the *angle
    /// conventions* are not interchangeable, and this is what stops a caller
    /// handing a wall banner a rotation segment.
    ///
    /// A segment is `22.5°` per step and a facing is `90°`, so segment `4` and
    /// facing `west` are the same `90°` rotation while segment `4` read as a
    /// *facing* would be nothing at all. The gate pins the shared shape and the
    /// distinct convention together: equal matrices at equal *angles*, and a
    /// deliberately unequal pair at the same numeric input.
    #[test]
    fn the_two_banner_placements_share_one_transform_and_two_angle_conventions() {
        let pos = [3, 70, -5];
        // Segment 4 is 4 * 22.5 = 90 degrees, which is also `west`'s toYRot.
        assert_eq!(
            banner_ground_placement_matrix(pos, 4),
            banner_wall_placement_matrix(pos, horizontal_facing_yaw("west").unwrap()),
            "one modelTransformation, two callers"
        );
        // The same *number* means different things to the two.
        assert_ne!(
            banner_ground_placement_matrix(pos, 4),
            banner_wall_placement_matrix(pos, 4.0),
            "a segment is 22.5 degrees per step; a facing yaw is degrees"
        );
        // And neither has skull's push away from the wall: the block's own
        // corner-plus-half is the whole translation.
        let at_origin = banner_wall_placement_matrix([0, 0, 0], 0.0).transform_point3(Vec3::ZERO);
        assert!(
            (at_origin - Vec3::new(0.5, 0.0, 0.5)).length() < 1e-6,
            "no extra offset away from the wall, got {at_origin}"
        );
    }

    /// Vanilla's own clockwise-rotated yaw, hand-expanded from the jar's own two
    /// tables (the clockwise-turn table: north→east→south→west→north, and
    /// the yaw table: south 0, west 90, north 180, east 270).
    ///
    /// The wrong hypothesis is not an error but a quarter turn, and it is
    /// spelled with the function *next to* the right one, so this asserts both
    /// arms in the same run: every facing's clockwise yaw must differ from its
    /// plain yaw by exactly 90°, and the four expected values are written out
    /// rather than derived from `horizontal_facing_yaw` (which would make the
    /// test agree with whatever the implementation does).
    #[test]
    fn a_lecterns_yaw_is_the_facing_turned_clockwise_not_the_facing() {
        for (facing, clockwise, plain) in [
            ("north", 270.0_f32, 180.0_f32),
            ("east", 0.0, 270.0),
            ("south", 90.0, 0.0),
            ("west", 180.0, 90.0),
        ] {
            assert_eq!(
                horizontal_facing_clockwise_yaw(facing),
                Some(clockwise),
                "{facing}"
            );
            assert_eq!(horizontal_facing_yaw(facing), Some(plain), "{facing}");
            assert_ne!(
                clockwise, plain,
                "{facing}: the two must differ, or this test proves nothing"
            );
        }
        assert_eq!(horizontal_facing_clockwise_yaw("up"), None);
    }

    /// Vanilla's own book animation-state constructor, called with
    /// `(0.0, 0.1, 0.9, 1.2)`, collapses to a
    /// constant, computed here from the jar's four literals rather than by
    /// reading [`LECTERN_BOOK_OPENNESS`] back.
    ///
    /// The point is the `sin(progress * 0.02)` term: it is dead at
    /// `progress == 0`, which is why a lectern book must not be given a live
    /// clock. The second assertion is the control — with a *non*-zero progress
    /// the same formula does move, so the constant is a property of the
    /// lectern's arguments and not of the formula being inert.
    #[test]
    fn a_lectern_books_openness_is_constant_because_its_progress_term_is_dead() {
        fn for_animation(progress: f32, openness: f32) -> f32 {
            ((progress * 0.02).sin() * 0.1 + 1.25) * openness
        }
        assert!((for_animation(0.0, 1.2) - LECTERN_BOOK_OPENNESS).abs() < 1e-6);
        assert!((for_animation(0.0, 1.2) - 1.5).abs() < 1e-6);
        assert!(
            (for_animation(100.0, 1.2) - 1.5).abs() > 1e-3,
            "a live progress *would* move openness, so the constant above is \
             about the lectern's own arguments"
        );
    }

    /// The six posed parts, against vanilla's own book animation update
    /// transcribed by hand.
    ///
    /// `seam` must be absent from the list: the jar never poses it, and its rest
    /// `rotation(0, PI/2, 0)` is the spine's quarter turn — an override with a
    /// zero `y_rot` would flatten it into the covers, which still draws a
    /// plausible book.
    #[test]
    fn the_books_six_poses_match_setup_anim_and_leave_the_seam_alone() {
        let openness = 1.5_f32;
        let poses = book_part_poses(openness, (0.1, 0.9));
        let by_name = |name: &str| {
            poses
                .iter()
                .find(|(n, _, _)| *n == name)
                .copied()
                .unwrap_or_else(|| panic!("{name} is not posed"))
        };

        let slide = openness.sin();
        for (name, expected_y_rot, expected_x) in [
            ("left_lid", std::f32::consts::PI + 1.5, None),
            ("right_lid", -1.5, None),
            ("left_pages", 1.5, Some(slide)),
            ("right_pages", -1.5, Some(slide)),
            // openness - openness*2*flip: 1.5 - 0.3 and 1.5 - 2.7.
            ("flip_page1", 1.2, Some(slide)),
            ("flip_page2", -1.2, Some(slide)),
        ] {
            let (_, y_rot, x) = by_name(name);
            assert!(
                (y_rot - expected_y_rot).abs() < 1e-5,
                "{name}: y_rot {y_rot} != {expected_y_rot}"
            );
            match (x, expected_x) {
                (Some(a), Some(b)) => assert!((a - b).abs() < 1e-6, "{name}: x"),
                (None, None) => {}
                _ => panic!("{name}: x presence"),
            }
        }
        assert!(
            !poses.iter().any(|(n, _, _)| *n == "seam"),
            "the seam is never posed by the jar"
        );

        // The two flip pages must land on opposite sides of the spine — that is
        // what makes a book look mid-turn rather than shut. A transcription that
        // dropped the `* 2` gives 1.35 and 0.15: both positive, same side, and a
        // sign-only assertion would pass.
        let (_, flip1, _) = by_name("flip_page1");
        let (_, flip2, _) = by_name("flip_page2");
        assert!(flip1 > 0.0 && flip2 < 0.0, "{flip1} / {flip2}");
    }

    /// The `67.5°` tilt about **Z** is what makes a lectern book face a reader,
    /// and it is the whole difference from [`block_entity_placement_matrix`].
    ///
    /// Expectation from the transform algebra, not from the implementation: `Ry`
    /// preserves a vector's `y` component, so the angle between the book's own
    /// up axis and world up is exactly the tilt for **every** facing. Reusing
    /// the chest placement matrix gives `0°` — the wrong hypothesis is computed
    /// here and required to be far away, in the same run.
    #[test]
    fn the_books_placement_tilts_it_by_the_jars_angle_at_every_facing() {
        let up = Vec3::Y;
        for facing in ["north", "east", "south", "west"] {
            let yaw = horizontal_facing_clockwise_yaw(facing).unwrap();
            let m = lectern_book_placement_matrix([3, 4, 5], yaw);
            let book_up = m.transform_vector3(up).normalize();
            let angle = book_up.dot(up).clamp(-1.0, 1.0).acos().to_degrees();
            assert!(
                (angle - 67.5).abs() < 1e-3,
                "{facing}: tilt {angle} != 67.5"
            );

            // The wrong hypothesis, in the same run.
            let flat = block_entity_placement_matrix([3, 4, 5], yaw);
            let flat_angle = flat
                .transform_vector3(up)
                .normalize()
                .dot(up)
                .clamp(-1.0, 1.0)
                .acos()
                .to_degrees();
            assert!(flat_angle < 1e-3, "the chest matrix does not tilt at all");
        }

        // The facing really does turn the book: opposite facings must put the
        // book's horizontal lean in opposite directions. A placement that
        // dropped the `Ry` term entirely would satisfy the tilt assertion above
        // at all four facings and fail here.
        let north = lectern_book_placement_matrix(
            [0, 0, 0],
            horizontal_facing_clockwise_yaw("north").unwrap(),
        )
        .transform_vector3(Vec3::Y);
        let south = lectern_book_placement_matrix(
            [0, 0, 0],
            horizontal_facing_clockwise_yaw("south").unwrap(),
        )
        .transform_vector3(Vec3::Y);
        let horizontal = |v: Vec3| Vec3::new(v.x, 0.0, v.z);
        assert!(
            horizontal(north).dot(horizontal(south)) < 0.0,
            "north {north} vs south {south}"
        );
    }

    /// The lectern reaches the batcher, batches on its own key, and the six
    /// overrides really are in the instance's `part_transforms`.
    ///
    /// The last part is the one a "does it draw" check misses: a book whose
    /// overrides were dropped is a *shut* book, which still batches, still
    /// culls, still draws, and still looks like a book from any distance.
    #[test]
    fn a_lectern_batches_on_its_own_key_with_its_overrides_applied() {
        let set = set();
        let mesh = set.get(BOOK).unwrap();
        let lectern = set.resolve_lectern(&LecternSpawn::at([0, 0, 0])).unwrap();
        assert_eq!(lectern.model, BOOK);
        assert_eq!(lectern.texture, BOOK_TEXTURE_STEM);
        assert_eq!(lectern.part_transforms.len(), mesh.parts.len());

        // Rest transforms through the *same* placement, so the only difference
        // between the two is the override list.
        let placement = lectern_book_placement_matrix([0, 0, 0], LecternSpawn::at([0; 3]).facing_yaw_deg);
        let rest = mesh.part_transforms(placement, &[]);
        for name in [
            "left_lid",
            "right_lid",
            "left_pages",
            "right_pages",
            "flip_page1",
            "flip_page2",
        ] {
            let i = mesh.index_of(name).unwrap();
            assert_ne!(
                rest[i], lectern.part_transforms[i],
                "{name} was not posed"
            );
        }
        // …and the seam is, correctly, untouched.
        let seam = mesh.index_of("seam").unwrap();
        assert_eq!(rest[seam], lectern.part_transforms[seam]);

        let chest = set.resolve_chest(&ChestSpawn::at([2, 0, 0])).unwrap();
        let cam = looking_at_origin();
        let frame = plan_block_entities(
            &[chest, lectern],
            &Frustum::from_view_projection(cam.view_projection()),
        );
        assert_eq!(frame.stats.drawn, 2);
        assert_eq!(frame.batches.len(), 2, "a book is its own model and sheet");
    }

/// Held/dropped `minecraft:special` item forms — [`special_item_rig`] and the rig
/// geometry it names.
///
/// Its own module because these gates are about the **item** surfaces (hand, drop,
/// item frame, inventory slot), not about a placed block entity, and the two
/// families fail for different reasons.
#[cfg(test)]
mod special_item_tests {
    use super::*;

    /// Every `(model, sheet)` pair [`special_item_rig`] can return must really
    /// exist: the model in [`BlockEntityModelSet`] and the sheet in the preload
    /// list the shell builds bind groups from.
    ///
    /// **This is the island check, and it is the one that matters most here.** A
    /// typo in a model name or a stem is not a compile error — both are
    /// `&'static str` — and the only symptom is a held chest that silently draws
    /// nothing, which is byte-for-byte the bug this whole path exists to fix. So the
    /// assertion is not "the mapping returns something", it is "what it returns can
    /// be looked up".
    ///
    /// The subjects are the real 26.2 item paths, one per resolving `kind`, plus the
    /// two ends of the shulker colour range and both skull rigs (the mob 32×32 canvas
    /// and the humanoid 64×64 one) — a single subject per kind would leave whichever
    /// arm picked the wrong canvas passing.
    #[test]
    fn every_rig_and_sheet_the_resolver_names_can_actually_be_looked_up() {
        let models = BlockEntityModelSet::load();
        let stems = block_entity_texture_stems();
        let mut wrong: Vec<String> = Vec::new();
        for (kind, path) in [
            ("minecraft:chest", "chest"),
            ("minecraft:chest", "trapped_chest"),
            ("minecraft:chest", "ender_chest"),
            ("minecraft:chest", "oxidized_copper_chest"),
            ("minecraft:shulker_box", "shulker_box"),
            ("minecraft:shulker_box", "white_shulker_box"),
            ("minecraft:shulker_box", "black_shulker_box"),
            // `skeleton_skull` and `creeper_head` are the 32x32 mob rig;
            // `zombie_head` and `player_head` are the 64x64 humanoid one.
            ("minecraft:head", "skeleton_skull"),
            ("minecraft:head", "wither_skeleton_skull"),
            ("minecraft:head", "creeper_head"),
            ("minecraft:head", "zombie_head"),
            ("minecraft:player_head", "player_head"),
            ("minecraft:shield", "shield"),
            ("minecraft:conduit", "conduit"),
            // Both ends of the statue oxidation range plus a waxed path, so an
            // arm that dropped the `waxed_` strip cannot pass by covering only
            // the four unwaxed names.
            ("minecraft:copper_golem_statue", "copper_golem_statue"),
            ("minecraft:copper_golem_statue", "oxidized_copper_golem_statue"),
            (
                "minecraft:copper_golem_statue",
                "waxed_weathered_copper_golem_statue",
            ),
        ] {
            let Some((model, stem)) = special_item_rig(kind, path) else {
                wrong.push(format!("{kind}/{path}: resolved to nothing"));
                continue;
            };
            if models.get(model).is_none() {
                wrong.push(format!(
                    "{kind}/{path}: model {model:?} is not in BLOCK_ENTITY_MODELS"
                ));
            }
            if !stems.contains(&stem) {
                wrong.push(format!(
                    "{kind}/{path}: sheet {stem:?} is not in the preload list, so the \
                     shell builds no bind group for it and this draws nothing"
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// Both halves of the two-level key are load-bearing: the `kind` picks the rig
    /// and the item path picks the sheet *within* it.
    ///
    /// The wrong hypothesis this excludes is "the item path alone is enough" — and
    /// the discriminating pair is a plain chest against a trapped one, which share a
    /// `kind` and a **mesh** and differ only in sheet. A resolver keyed on `kind`
    /// alone passes any "does it resolve" check and draws every trapped chest with
    /// the plain sheet.
    #[test]
    fn the_kind_picks_the_rig_and_the_item_path_picks_the_sheet() {
        let (plain_model, plain_stem) =
            special_item_rig("minecraft:chest", "chest").expect("a plain chest");
        let (trapped_model, trapped_stem) =
            special_item_rig("minecraft:chest", "trapped_chest").expect("a trapped chest");
        assert_eq!(
            plain_model, trapped_model,
            "the two share one mesh; only the sheet differs"
        );
        assert_ne!(
            plain_stem, trapped_stem,
            "keying the sheet on `kind` alone draws every trapped chest plain"
        );
        assert_eq!(plain_model, CHEST_SINGLE);

        // And the reciprocal: one item path under two different `kind`s must not
        // collapse. `player_head` is its own `kind` in vanilla precisely because its
        // renderer resolves a profile texture.
        assert_eq!(
            special_item_rig("minecraft:player_head", "player_head"),
            special_item_rig("minecraft:head", "player_head"),
            "the two head kinds share one rig family here — we fetch no profile skin, \
             so a player head draws the default sheet exactly as a placed one does"
        );
    }

    /// A held chest is always the **single** chest layer, never a double half.
    ///
    /// Vanilla's own unbaked chest special renderer's `chest_type` defaults to SINGLE
    /// and no 26.2 item definition overrides it. The two double halves are 15 texels
    /// wide against the single's 14 and each omits the face meeting its partner, so
    /// picking one of those for an item leaves a chest with a hole in its side — and
    /// it still passes any "does a chest draw" gate.
    #[test]
    fn an_item_chest_is_the_single_layer_not_a_double_half() {
        let (model, _) = special_item_rig("minecraft:chest", "chest").expect("a chest");
        assert_eq!(model, CHEST_SINGLE);
        assert_ne!(model, CHEST_LEFT);
        assert_ne!(model, CHEST_RIGHT);
    }

    /// A dropped/framed/other-entity's-hand shield now resolves to the real rig and
    /// the **no-pattern** sheet specifically — never [`SHIELD_BASE_TEXTURE_STEM`],
    /// which would draw an opaque canvas meant to sit *under* a translucent
    /// dye/pattern layer this resolver never issues. Getting that backwards would
    /// still "resolve" (both stems are in the preload list) and still pass the
    /// corpus-wide lookup gate above, so this checks the sheet by name rather than
    /// merely that one was returned.
    #[test]
    fn shield_resolves_to_the_no_pattern_rig_and_sheet() {
        let (model, stem) =
            special_item_rig("minecraft:shield", "shield").expect("a shield now resolves here");
        assert_eq!(model, SHIELD);
        assert_eq!(
            stem, SHIELD_BASE_NO_PATTERN_TEXTURE_STEM,
            "a shield with no runtime dye/pattern state reaching this resolver must \
             draw the opaque no-pattern sheet, not the sheet meant to sit under a \
             translucent layer this resolver never issues"
        );
        assert_ne!(
            stem, SHIELD_BASE_TEXTURE_STEM,
            "the two sheets differ (the shield-bug fix's own 200-texel measurement), \
             so drawing the wrong one is a real, visible regression, not a rename"
        );
    }

    /// A conduit item resolves to the **shell** layer specifically, and the shell is
    /// six model units across rather than a full block.
    ///
    /// The quad count cannot carry this one, and that is the whole reason this gate
    /// measures a size instead. `conduit_shell_model` is a single
    /// `addBox(-3, -3, -3, 6, 6, 6)`, so it bakes to **6** quads — exactly what a
    /// plain block-item cube bakes to. A `== 6` assertion would therefore pass for
    /// the wrong hypothesis it exists to exclude, the coincident-input trap in the
    /// evidence rules. The *extent* separates them cleanly:
    ///
    /// | hypothesis | span, block units |
    /// |---|---|
    /// | the flat `base` sprite fallback | `0` (no `elements`) |
    /// | a plain block-item cube | `1.0` |
    /// | **the conduit shell** | **`0.375`** (`6 / 16`) |
    ///
    /// The cage is the other thing this could wrongly resolve to, and it is the
    /// plausible wrong pick rather than a strawman: it is the same one-box shape
    /// under a different name, so it passes any count *and* any "did it resolve"
    /// check, and differs only in being `8` units (`0.5`) on the `entity/conduit/cage`
    /// sheet. An item conduit is never active, so the shell is the only right answer.
    #[test]
    fn a_conduit_item_is_the_inactive_shell_layer_at_six_model_units() {
        let (model, stem) =
            special_item_rig("minecraft:conduit", "conduit").expect("a conduit item");
        assert_eq!(model, CONDUIT_SHELL);
        assert_eq!(stem, CONDUIT_SHELL_TEXTURE_STEM);
        assert_ne!(
            model, CONDUIT_CAGE,
            "the cage is the active shell; an item conduit is never active, and the \
             two bake to the same quad count so only the name and size tell them apart"
        );

        let models = BlockEntityModelSet::load();
        let mesh = models.get(model).expect("the conduit shell mesh");
        let span = mesh.local_max - mesh.local_min;
        for (axis, value) in [("x", span.x), ("y", span.y), ("z", span.z)] {
            assert!(
                (value - 0.375).abs() < 1e-5,
                "conduit shell {axis} span was {value}, not the 6/16 blocks \
                 `createShellLayer`'s addBox(-3, -3, -3, 6, 6, 6) gives — 1.0 would \
                 mean a plain block cube and 0.5 would mean the cage"
            );
        }

        // The name assertion above fires *before* the size loop if the arm is
        // repointed, so on its own the size loop is an argument rather than an
        // observation. This measures the cage directly, which is what makes the
        // 0.375 predicate discriminating rather than merely true: the two meshes
        // bake to the same 6 quads and differ only here.
        let cage = models.get(CONDUIT_CAGE).expect("the conduit cage mesh");
        let cage_span = cage.local_max - cage.local_min;
        assert!(
            (cage_span.x - 0.5).abs() < 1e-5,
            "the cage measured {cage_span:?}, not the 8/16 blocks \
             `createCageLayer`'s addBox(-4, -4, -4, 8, 8, 8) gives — if the two \
             layers ever share a span, the size predicate above stops separating them"
        );
        assert_ne!(
            mesh.quad_count(),
            0,
            "the vacuous base-sprite fallback"
        );
        assert_eq!(
            mesh.quad_count(),
            cage.quad_count(),
            "shell and cage are both one box, so a quad count cannot tell them \
             apart — this is why the assertion above is a span"
        );
    }

    /// Every copper golem statue item path resolves to the **standing** rig, and the
    /// eight paths collapse onto exactly four sheets.
    ///
    /// Two wrong hypotheses are named rather than described. Keying the sheet on the
    /// `kind` alone draws all eight unaffected-copper, so the four unwaxed paths must
    /// produce four *distinct* stems. Forgetting the `waxed_` strip makes the four
    /// waxed paths resolve to nothing — a statue that vanishes for half the family —
    /// so each waxed path must equal its unwaxed twin exactly.
    ///
    /// The pose is asserted as standing for all eight because an item stack carries
    /// no `copper_golem_pose` property, which is what vanilla's own `select`
    /// fallback does; picking any other pose would still resolve and still draw a
    /// statue, so this is checked by name.
    #[test]
    fn a_statue_item_is_always_standing_and_its_eight_paths_are_four_sheets() {
        let mut stems = Vec::new();
        for path in [
            "copper_golem_statue",
            "exposed_copper_golem_statue",
            "weathered_copper_golem_statue",
            "oxidized_copper_golem_statue",
        ] {
            let (model, stem) = special_item_rig("minecraft:copper_golem_statue", path)
                .unwrap_or_else(|| panic!("{path} must resolve"));
            assert_eq!(
                model,
                CopperGolemPose::Standing.model_name(),
                "{path} took a pose no item stack can ask for"
            );

            let waxed = format!("waxed_{path}");
            assert_eq!(
                special_item_rig("minecraft:copper_golem_statue", &waxed),
                Some((model, stem)),
                "{waxed} must fold onto {path} — waxing halts weathering but changes \
                 no sheet, and dropping the strip makes four of the eight draw nothing"
            );
            stems.push(stem);
        }

        let mut distinct = stems.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            4,
            "the four oxidation levels collapsed to {distinct:?} — keying the sheet on \
             the `kind` alone draws every statue unaffected-copper"
        );
    }

    /// [`decorated_pot_item_rig`] names five real meshes and five real sheets, an
    /// undecorated pot takes the default side sprite on all four faces rather than
    /// dropping them, and a decorated one puts a **distinct** sheet on each face.
    ///
    /// The distinctness arm is the one that matters, and it is not a tautology: the
    /// four faces share one mesh *shape* and differ only in which sherd sprite they
    /// sample, so a rig that returned the same stem four times would resolve, draw a
    /// complete pot, and be wrong in exactly the way nobody looks for. Four different
    /// sherds are passed for that reason — passing the same sherd twice is the
    /// coincident input that would let a transposed pair through.
    ///
    /// The face-to-argument mapping is checked by giving each face a sherd whose
    /// stem names it, because `PotDecorations`' record order is `back, left, right,
    /// front` while the *draw* order is base, front, back, left, right — two
    /// same-typed sequences in different orders, which is precisely the transposition
    /// this repo's rules say survives every round trip.
    #[test]
    fn a_pot_rig_sheets_four_faces_independently_and_defaults_the_blank_ones() {
        let models = BlockEntityModelSet::load();
        let stems = block_entity_texture_stems();

        // Undecorated: every face draws, and draws the default sprite.
        let plain = decorated_pot_item_rig(None, None, None, None);
        let mut wrong: Vec<String> = Vec::new();
        for (model, stem) in plain.parts() {
            if models.get(model).is_none() {
                wrong.push(format!("model {model:?} is not in BLOCK_ENTITY_MODELS"));
            }
            if !stems.contains(&stem) {
                wrong.push(format!("sheet {stem:?} is not in the preload list"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
        for (model, stem) in [plain.front, plain.back, plain.left, plain.right] {
            assert_eq!(
                stem, DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM,
                "{model} skipped its blank face instead of drawing the default \
                 sprite — vanilla's submit calls submitModelPart for all four \
                 unconditionally, and skipping silently autocorrects the moment a \
                 player adds their first sherd"
            );
        }
        assert_eq!(
            plain.base,
            (DECORATED_POT_BASE, DECORATED_POT_BASE_TEXTURE_STEM),
            "the body is always the base sheet, whatever the sides carry"
        );

        // Decorated: four different sherds, one per face, checked by name so a
        // transposition between the record order and the draw order cannot pass.
        let decorated = decorated_pot_item_rig(
            Some("angler_pottery_sherd"),
            Some("blade_pottery_sherd"),
            Some("burn_pottery_sherd"),
            Some("danger_pottery_sherd"),
        );
        for (face, got, sherd) in [
            ("back", decorated.back.1, "angler"),
            ("left", decorated.left.1, "blade"),
            ("right", decorated.right.1, "burn"),
            ("front", decorated.front.1, "danger"),
        ] {
            assert!(
                got.contains(sherd),
                "the {face} face took {got:?}, which is not the {sherd} sherd it was \
                 given — `PotDecorations` orders its fields back/left/right/front \
                 while the draws go base/front/back/left/right, so a transposition \
                 here still draws a complete pot"
            );
        }
        let mut sheets = [
            decorated.front.1,
            decorated.back.1,
            decorated.left.1,
            decorated.right.1,
        ];
        sheets.sort_unstable();
        let distinct = {
            let mut s = sheets.to_vec();
            s.dedup();
            s.len()
        };
        assert_eq!(
            distinct, 4,
            "four different sherds collapsed onto {sheets:?} — a rig returning one \
             stem for every face draws a complete, uniformly wrong pot"
        );

        // An unrecognised sherd falls back for that face **only**, unlike
        // `special_item_rig`'s decline-the-whole-item contract. See the rig's own
        // doc for why the two differ.
        let datapack = decorated_pot_item_rig(Some("mypack:teapot_sherd"), None, None, None);
        assert_eq!(datapack.back.1, DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM);
        assert_eq!(
            datapack.base,
            plain.base,
            "one unknown sherd must not cost the pot its body"
        );
    }

    /// [`trident_item_rig`] names an entry that really exists **in the entity
    /// corpus**, and one whose geometry is the trident's own rather than the arrow
    /// rig it sits beside.
    ///
    /// This is the island check for the trident, and it is a different one from
    /// every other rig in this module because the failure mode is different. A
    /// chest's name is wrong-or-right within one corpus; the trident's name is a
    /// `&'static str` that would look equally plausible in *either*, and looking it
    /// up in [`BLOCK_ENTITY_MODELS`] — which does not hold it — returns `None` and
    /// draws precisely the empty hand this rig exists to fill. So the assertion is
    /// "absent from one corpus, present in the other", both directions stated,
    /// rather than "it resolves somewhere".
    ///
    /// The quad count is the magnitude half. `trident` and `arrow` are the two
    /// projectile rigs and a mislookup between them resolves, draws, and looks like
    /// a texture bug — `entity.rs`'s own corpus gate already pins that they differ,
    /// and this restates it at the item surface so a future corpus edit that
    /// collapsed them would fail here too rather than only there.
    #[test]
    fn the_trident_rig_lives_in_the_entity_corpus_and_is_not_the_arrow() {
        let entry = trident_item_rig("trident").expect("a trident item");
        assert_eq!(entry, TRIDENT_ENTITY_MODEL);
        assert_eq!(
            trident_item_rig("not_a_trident"),
            None,
            "a datapack item naming this kind over something else must draw nothing"
        );

        let block_entities = BlockEntityModelSet::load();
        assert!(
            block_entities.get(entry).is_none(),
            "the trident is in BLOCK_ENTITY_MODELS after all — if it moved there, \
             `trident_item_rig`'s whole reason for being a separate entry point is \
             gone and the hand's entity-corpus lookup is now the wrong one"
        );

        let entities = crate::entity::EntityModelSet::load();
        let trident = entities
            .get(entry)
            .unwrap_or_else(|| panic!("{entry:?} is not in the entity corpus either, so \
                 the held trident resolves to nothing and draws an empty hand"));
        let arrow = entities.get("arrow").expect("the arrow rig");
        assert_ne!(
            trident.quad_count(),
            arrow.quad_count(),
            "trident and arrow bake to the same quad count, so a mislookup between \
             the two would resolve and draw and read as a texture bug"
        );
        assert!(
            trident.quad_count() > 0,
            "an empty mesh uploads no part ranges, and `build_entity_rig_hand_draw` \
             returns None for that — an empty hand wearing a resolved rig's name"
        );
    }

    /// The `kind`s that need more than one `(model, sheet)` pair, and the item paths a
    /// `kind` must decline, resolve to nothing rather than to a plausible wrong rig.
    /// `shield` is deliberately **not** in this list any more — see
    /// [`special_item_rig`]'s own doc for why it resolves here too (always
    /// undyed/pattern-less), and [`shield_resolves_to_the_no_pattern_rig_and_sheet`]
    /// for its own positive gate.
    ///
    /// **`conduit` and `copper_golem_statue` are no longer in it either**, and their
    /// removal is the point of this edit rather than a side effect: both are a plain
    /// pair and always were, so their `None` was a resolver gap and not an unported
    /// rig. Only the *undeclared-path* arms for those two `kind`s stay, which is what
    /// keeps a datapack item from quietly becoming an unaffected-copper statue.
    ///
    /// **`dragon_head`/`piglin_head` are no longer in it, and their removal is
    /// the sharp lesson here rather than a side effect.** They were this gate's
    /// stand-in for "a real `minecraft:head` item whose rig is unported", which
    /// is a premise with an expiry date nothing tracked: porting the two rigs
    /// made the arm assert the opposite of the truth, and only the resulting
    /// red told anyone. Their positive gate is
    /// [`every_head_item_path_resolves_to_its_own_rig_and_sheet`]. What stays is
    /// the *undeclared-path* arm (`minecraft:head` over `stone`), which is a
    /// property of the resolver rather than of what happens to be ported.
    #[test]
    fn unported_kinds_and_undeclared_paths_resolve_to_nothing() {
        let mut wrong: Vec<String> = Vec::new();
        for (kind, path) in [
            ("minecraft:banner", "white_banner"),
            ("minecraft:decorated_pot", "decorated_pot"),
            ("minecraft:trident", "trident"),
            // A datapack item declaring one of the two `kind`s that *do* resolve
            // here now, over something that is not one of their real paths.
            ("minecraft:conduit", "not_a_conduit"),
            ("minecraft:copper_golem_statue", "iron_golem_statue"),
            // A datapack item declaring a `kind` over something that is not one.
            ("minecraft:chest", "diamond_pickaxe"),
            ("minecraft:shulker_box", "not_a_shulker_box"),
            ("minecraft:head", "stone"),
            // An unknown kind entirely.
            ("mypack:teapot", "teapot"),
        ] {
            if let Some(rig) = special_item_rig(kind, path) {
                wrong.push(format!("{kind}/{path}: resolved to {rig:?}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// Every one of vanilla's seven head items resolves through
    /// [`special_item_rig`] to its own `(rig, sheet)` pair, and the seven pairs
    /// are **distinct**. The distinctness is the assertion that matters: a
    /// resolver that fell through to any single skull rig would draw a dragon
    /// head as a skeleton skull, which reads as a texture bug rather than as a
    /// missing rig, and a coverage-only "all seven resolve" check passes for it.
    #[test]
    fn every_head_item_path_resolves_to_its_own_rig_and_sheet() {
        let paths = [
            "skeleton_skull",
            "wither_skeleton_skull",
            "zombie_head",
            "creeper_head",
            "player_head",
            "dragon_head",
            "piglin_head",
        ];
        let mut missing: Vec<&str> = Vec::new();
        let mut rigs = Vec::new();
        for path in paths {
            match special_item_rig("minecraft:head", path) {
                Some(rig) => rigs.push(rig),
                None => missing.push(path),
            }
        }
        assert!(missing.is_empty(), "did not resolve: {missing:?}");
        let unique: std::collections::BTreeSet<_> = rigs.iter().collect();
        assert_eq!(unique.len(), paths.len(), "collapsed onto one rig: {rigs:?}");
        // The two that share no geometry with the 8x8x8 box must reach their
        // own models by name, not merely a distinct sheet on a shared rig.
        assert_eq!(
            special_item_rig("minecraft:head", "dragon_head"),
            Some((SKULL_DRAGON, "entity/enderdragon/dragon"))
        );
        assert_eq!(
            special_item_rig("minecraft:head", "piglin_head"),
            Some((SKULL_PIGLIN, "entity/piglin/piglin"))
        );
    }

    /// **The geometry gate**: a chest rig is `18` quads — three boxes of six faces —
    /// which is what tells a real rig from the two things that look like a fix and
    /// are not.
    ///
    /// | hypothesis | quads |
    /// |---|---|
    /// | the flat `base` sprite fallback | `0` (the base model has no `elements` and no `layer0`) |
    /// | a plain block-item cube | `6` |
    /// | **the chest rig** | **`18`** |
    ///
    /// A presence-only assertion ("something drew") is satisfied by the first two,
    /// and the first is exactly what the GUI's own doc measured as *vacuous*. Both
    /// wrong values are named here rather than described, so the predicate is a
    /// prediction and not a sign test.
    ///
    /// The shulker box is checked too, and for a reason specific to it: a closed
    /// shulker box is **near-cubic**, so if it were the only subject a cube-shaped
    /// fallback would be hard to tell from the rig by silhouette. Its quad count
    /// still separates them.
    #[test]
    fn the_chest_rig_has_a_quad_count_no_sprite_or_cube_fallback_can_produce() {
        let models = BlockEntityModelSet::load();
        let chest = models.get(CHEST_SINGLE).expect("the single chest layer");
        assert_eq!(
            chest.quad_count(),
            18,
            "the single chest is bottom + lid + lock, three boxes of six faces"
        );
        assert_ne!(chest.quad_count(), 6, "a plain block-item cube");
        assert_ne!(chest.quad_count(), 0, "the vacuous base-sprite fallback");

        let shulker = models.get(SHULKER_BOX).expect("the shulker box layer");
        assert_ne!(shulker.quad_count(), 6, "a plain block-item cube");
        assert_ne!(shulker.quad_count(), 0, "the vacuous base-sprite fallback");
    }

    /// A chest's parts are **posed relative to each other**, which is the property a
    /// single-cube fallback structurally cannot have — and the reason a chest rather
    /// than a shulker box is the right subject for this gate.
    ///
    /// Predicted exactly, from `chest_single_model`'s own `PartPose::offset` values:
    /// `bottom` is `PartPose::ZERO`, so its matrix is the placement unchanged, while
    /// `lid` and `lock` are both `offset(0, 9, 1)` in texels — `9/16` up and `1/16`
    /// forward in block-local space. Not "the matrices differ", which a jitter would
    /// satisfy: the exact translation, derived from the model definition rather than
    /// restated as a number.
    #[test]
    fn the_chest_parts_are_posed_relative_to_the_placement_not_stacked_on_it() {
        let models = BlockEntityModelSet::load();
        let chest = models.get(CHEST_SINGLE).expect("the single chest layer");
        let placement = Mat4::from_translation(Vec3::new(3.0, 5.0, 7.0));
        let transforms = chest.part_transforms(placement, &[]);

        let bottom = chest.index_of("bottom").expect("a `bottom` part");
        let lid = chest.index_of("lid").expect("a `lid` part");
        let lock = chest.index_of("lock").expect("a `lock` part");

        let origin = |i: usize| transforms[i].transform_point3(Vec3::ZERO);
        let placed = placement.transform_point3(Vec3::ZERO);
        assert!(
            (origin(bottom) - placed).length() < 1e-5,
            "`bottom` is PartPose::ZERO, so it must be the placement unchanged; got {}",
            origin(bottom)
        );
        // `PartPose::offset(0.0, 9.0, 1.0)` in texels, and the mesh is block-local.
        let expected = placed + Vec3::new(0.0, 9.0 / 16.0, 1.0 / 16.0);
        let mut wrong: Vec<String> = Vec::new();
        for (name, index) in [("lid", lid), ("lock", lock)] {
            let got = origin(index);
            if (got - expected).length() > 1e-5 {
                wrong.push(format!("{name}: expected {expected}, got {got}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
        // And the control: the offset is big enough that "all parts share the
        // placement" — the single-cube hypothesis — fails the assertion above.
        assert!(
            (expected - placed).length() > 0.1,
            "control failed: the lid offset is too small to distinguish a real rig \
             from three copies of one cube"
        );
    }

    /// [`BlockEntityModelSet::resolve_special_item`] is what the three world
    /// surfaces (dropped stack, another entity's hand, item frame) each turn a
    /// placement into an instance with, so it has to carry the **whole rig** and be
    /// keyed on the sheet the batcher groups by.
    ///
    /// The geometry predicate is the one from
    /// `the_chest_rig_has_a_quad_count_no_sprite_or_cube_fallback_can_produce`,
    /// applied through this accessor: `18` quads and `3` parts, against `6` for a
    /// block-item cube and `0`/`1` for the sprite fallback. A presence assertion is
    /// satisfied by both wrong answers, which is exactly why the held chest looked
    /// fine in the GUI for so long.
    #[test]
    fn resolve_special_item_carries_the_whole_rig_and_the_items_own_sheet() {
        let models = BlockEntityModelSet::load();
        let placement = Mat4::from_translation(Vec3::new(12.0, 70.0, -4.0));
        let chest = models
            .resolve_special_item("minecraft:chest", "chest", placement, &[], 0x8F)
            .expect("a plain chest resolves");
        assert_eq!(chest.model, CHEST_SINGLE);
        // **Four**, not three. `chest_single_model`'s root is a real, geometry-free
        // `PartDef` and `bake_entity_parts` emits it under an empty name, so the
        // part list is `["", "bottom", "lid", "lock"]` — the count asserted by
        // `single_chest_has_the_three_vanilla_parts_in_order` in `lodestone-assets`.
        // "Three boxes, so three parts" is the plausible wrong number, and it fails
        // here rather than showing up as a missing lid.
        assert_eq!(
            chest.part_transforms.len(),
            4,
            "an empty-named root plus bottom + lid + lock; a cube fallback has one \
             part and a sprite none"
        );
        let mesh = models.get(chest.model).expect("the resolved mesh");
        assert_eq!(mesh.quad_count(), 18, "three boxes of six faces");
        assert_ne!(mesh.quad_count(), 6, "a plain block-item cube");
        assert_ne!(mesh.quad_count(), 0, "the vacuous base-sprite fallback");
        assert_eq!(chest.light, 0x8F, "the caller's light must ride through");

        // The AABB is the rig's own, moved by the placement — non-degenerate and
        // straddling the placement's translation. A zero-volume box would cull the
        // instance on the first frustum test and read as "nothing draws".
        let volume = (chest.aabb_max - chest.aabb_min).min_element();
        assert!(volume > 0.0, "degenerate AABB {:?}", chest.aabb_max - chest.aabb_min);
        let placed = placement.transform_point3(Vec3::ZERO);
        assert!(
            chest.aabb_min.x <= placed.x + 1.0 && chest.aabb_max.x >= placed.x,
            "the AABB {:?}..{:?} does not follow the placement at {placed}",
            chest.aabb_min,
            chest.aabb_max
        );

        // The **sheet** is what the batcher keys on alongside the model, so a
        // trapped chest must share the mesh and differ here. Collected, so one
        // wrong arm does not hide the other.
        let trapped = models
            .resolve_special_item("minecraft:chest", "trapped_chest", placement, &[], 0)
            .expect("a trapped chest resolves");
        let mut wrong: Vec<String> = Vec::new();
        if trapped.model != chest.model {
            wrong.push(format!(
                "a trapped chest took a different mesh ({}) from a plain one ({})",
                trapped.model, chest.model
            ));
        }
        if trapped.texture == chest.texture {
            wrong.push(format!(
                "a trapped chest shares the plain chest's sheet ({})",
                chest.texture
            ));
        }
        // And the negative control that must fire: a `kind` this entry point
        // cannot express, or a path its `kind` declines, yields nothing rather
        // than a default oak chest. `conduit` is deliberately *not* here any
        // more — it is a plain one-mesh pair and resolves through this path now;
        // its undeclared-path arm below is what still has to decline.
        for (kind, path) in [
            ("minecraft:trident", "trident"),
            ("minecraft:decorated_pot", "decorated_pot"),
            ("minecraft:conduit", "not_a_conduit"),
            ("minecraft:chest", "diamond_pickaxe"),
            ("mypack:teapot", "teapot"),
        ] {
            if models
                .resolve_special_item(kind, path, placement, &[], 0)
                .is_some()
            {
                wrong.push(format!("{kind}/{path} resolved to an instance"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// Every stored sherd this table names resolves to a **distinct** pattern
    /// stem, and an unrecognised path (a datapack item, or `None`'s own
    /// caller-side default) resolves to nothing rather than a plausible wrong
    /// sherd — the same "decline rather than guess" contract
    /// [`special_item_rig`]'s own tests hold `kind` to.
    #[test]
    fn every_named_sherd_resolves_to_a_distinct_pattern_stem() {
        let sherds = [
            "angler_pottery_sherd",
            "archer_pottery_sherd",
            "arms_up_pottery_sherd",
            "blade_pottery_sherd",
            "brewer_pottery_sherd",
            "burn_pottery_sherd",
            "danger_pottery_sherd",
            "explorer_pottery_sherd",
            "flow_pottery_sherd",
            "friend_pottery_sherd",
            "guster_pottery_sherd",
            "heart_pottery_sherd",
            "heartbreak_pottery_sherd",
            "howl_pottery_sherd",
            "miner_pottery_sherd",
            "mourner_pottery_sherd",
            "plenty_pottery_sherd",
            "prize_pottery_sherd",
            "scrape_pottery_sherd",
            "sheaf_pottery_sherd",
            "shelter_pottery_sherd",
            "skull_pottery_sherd",
            "snort_pottery_sherd",
        ];
        let mut stems: Vec<&'static str> = Vec::with_capacity(sherds.len());
        for sherd in sherds {
            let stem = decorated_pot_pattern_texture_stem(sherd)
                .unwrap_or_else(|| panic!("{sherd} must resolve to a pattern stem"));
            assert!(
                stem.starts_with("entity/decorated_pot/") && stem.ends_with("_pottery_pattern"),
                "{sherd} resolved to {stem}, which does not look like a decorated-pot pattern"
            );
            stems.push(stem);
        }
        let mut deduped = stems.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(
            deduped.len(),
            stems.len(),
            "two different sherds resolved to the same pattern stem: {stems:?}"
        );

        // Absence and the "not a real sherd" case both decline, per the
        // function's own contract — a resolver that fell back to a plausible
        // pattern here would draw a datapack item as a real vanilla sherd.
        assert_eq!(decorated_pot_pattern_texture_stem("brick"), None);
        assert_eq!(decorated_pot_pattern_texture_stem("diamond_pickaxe"), None);
        assert_eq!(decorated_pot_pattern_texture_stem(""), None);
    }

    /// An undecorated pot resolves to five instances — the base plus all
    /// four sides, every side falling back to
    /// [`DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM`] — matching
    /// vanilla's own decorated-pot renderer's own unconditional four
    /// part-submit calls: a blank side is drawn with the default
    /// sprite, not skipped.
    #[test]
    fn an_undecorated_pot_resolves_to_a_base_and_four_default_sides() {
        let models = BlockEntityModelSet::load();
        let spawn = DecoratedPotSpawn::at([5, 70, -3]);
        let [base, front, back, left, right] = models
            .resolve_decorated_pot(&spawn)
            .expect("the decorated-pot corpus must resolve");

        assert_eq!(base.model, DECORATED_POT_BASE);
        assert_eq!(base.texture, DECORATED_POT_BASE_TEXTURE_STEM);

        for (name, inst, model) in [
            ("front", &front, DECORATED_POT_SIDE_FRONT),
            ("back", &back, DECORATED_POT_SIDE_BACK),
            ("left", &left, DECORATED_POT_SIDE_LEFT),
            ("right", &right, DECORATED_POT_SIDE_RIGHT),
        ] {
            assert_eq!(inst.model, model, "{name} took the wrong model");
            assert_eq!(
                inst.texture, DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM,
                "{name} of a blank pot must draw the default side sprite"
            );
        }
    }

    /// **The discriminating gate.** Four *pairwise-distinct* sherds, one per
    /// side, must resolve to four distinct textures **on the correct named
    /// model** — not merely "four different textures somewhere". Checking by
    /// `model` rather than by position is what makes this transposition-proof
    /// per `CLAUDE.md`'s evidence standard: a resolver that swapped, say,
    /// `left` and `right` would still produce "four distinct textures" but
    /// would fail this assertion, where a plain `HashSet::len() == 4` check
    /// would not catch it.
    ///
    /// The four sherds are chosen pairwise-distinct on purpose (never reusing
    /// one sherd across two sides) — the same discipline `CLAUDE.md` requires
    /// of adjacent same-typed fields, because a fixture with a repeated sherd
    /// cannot distinguish "resolved independently" from "one sherd painted
    /// everywhere".
    #[test]
    fn four_distinct_sherds_produce_four_distinct_textures_on_the_right_faces() {
        let models = BlockEntityModelSet::load();
        let spawn = DecoratedPotSpawn {
            front: Some("angler_pottery_sherd".to_string()),
            back: Some("skull_pottery_sherd".to_string()),
            left: Some("heart_pottery_sherd".to_string()),
            right: Some("danger_pottery_sherd".to_string()),
            ..DecoratedPotSpawn::at([1, 64, 1])
        };
        let [base, front, back, left, right] = models
            .resolve_decorated_pot(&spawn)
            .expect("the decorated-pot corpus must resolve");

        let expect = [
            ("front", &front, DECORATED_POT_SIDE_FRONT, "entity/decorated_pot/angler_pottery_pattern"),
            ("back", &back, DECORATED_POT_SIDE_BACK, "entity/decorated_pot/skull_pottery_pattern"),
            ("left", &left, DECORATED_POT_SIDE_LEFT, "entity/decorated_pot/heart_pottery_pattern"),
            ("right", &right, DECORATED_POT_SIDE_RIGHT, "entity/decorated_pot/danger_pottery_pattern"),
        ];
        let mut wrong: Vec<String> = Vec::new();
        for (name, inst, model, texture) in expect {
            if inst.model != model {
                wrong.push(format!("{name}: expected model {model}, got {}", inst.model));
            }
            if inst.texture != texture {
                wrong.push(format!("{name}: expected texture {texture}, got {}", inst.texture));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");

        // The base never varies with the sides.
        assert_eq!(base.model, DECORATED_POT_BASE);
        assert_eq!(base.texture, DECORATED_POT_BASE_TEXTURE_STEM);

        // Pairwise-distinct textures, not merely four non-default ones — a
        // resolver that mapped every side through the same lookup bug (e.g.
        // always the *first* sherd) would still clear the "not default"
        // check but fail this one.
        let textures = [front.texture, back.texture, left.texture, right.texture];
        for i in 0..textures.len() {
            for j in (i + 1)..textures.len() {
                assert_ne!(
                    textures[i], textures[j],
                    "sides at index {i} and {j} share a texture: {:?}",
                    textures
                );
            }
        }
    }

    /// A partially-decorated pot: only two of the four sides carry a sherd.
    /// The undecorated pair must still fall back to the default sprite while
    /// the decorated pair keeps its own — proving the fallback is per-side,
    /// not all-or-nothing.
    #[test]
    fn a_partially_decorated_pot_mixes_sherds_and_the_default_sprite() {
        let models = BlockEntityModelSet::load();
        let spawn = DecoratedPotSpawn {
            front: Some("brewer_pottery_sherd".to_string()),
            back: None,
            left: None,
            right: Some("miner_pottery_sherd".to_string()),
            ..DecoratedPotSpawn::at([0, 64, 0])
        };
        let [_base, front, back, left, right] = models
            .resolve_decorated_pot(&spawn)
            .expect("the decorated-pot corpus must resolve");

        assert_eq!(front.texture, "entity/decorated_pot/brewer_pottery_pattern");
        assert_eq!(right.texture, "entity/decorated_pot/miner_pottery_pattern");
        assert_eq!(back.texture, DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM);
        assert_eq!(left.texture, DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM);
    }

    /// [`decorated_pot_texture_stems`] must carry the base, the default side,
    /// and all twenty-three named patterns — the set
    /// [`block_entity_texture_stems`] (and so the shell's GPU texture loader)
    /// has to iterate for a pot with any combination of sherds to always find
    /// a bind group.
    #[test]
    fn decorated_pot_texture_stems_covers_the_base_default_and_every_pattern() {
        let stems = decorated_pot_texture_stems();
        assert!(stems.contains(&DECORATED_POT_BASE_TEXTURE_STEM));
        assert!(stems.contains(&DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM));
        assert_eq!(
            stems.len(),
            25,
            "expected base + default side + 23 sherd patterns, got {stems:?}"
        );
        assert!(block_entity_texture_stems().contains(&DECORATED_POT_BASE_TEXTURE_STEM));
    }

    /// The placement matrix's rotation term carries the `180°` offset chest's
    /// does not, and the pivot is the block's centre rather than its floor —
    /// both measured directly rather than merely "the pot draws somewhere",
    /// per [`decorated_pot_placement_matrix`]'s own doc on why this is not
    /// [`block_entity_placement_matrix`] with a yaw.
    #[test]
    fn decorated_pot_placement_uses_the_centre_pivot_and_the_180_degree_term() {
        let m = decorated_pot_placement_matrix([2, 5, 9], 0.0);
        // At `facing_yaw_deg == 0`, the rotation term is `180° - 0 = 180°`, a
        // half-turn about the block's own vertical centre line — so a point
        // on the +X face of the unit cube (local `(1, 0.5, 0.5)`) must land on
        // the -X side of the block after placement, not back on the +X side
        // the way an identity or a `0°` rotation would leave it.
        let p = m.transform_point3(Vec3::new(1.0, 0.5, 0.5));
        let origin_centre = Vec3::new(2.5, 5.5, 9.5);
        assert!(
            p.x < origin_centre.x,
            "expected the 180-degree term to flip +X across the block's centre; \
             got {p}, centre {origin_centre}"
        );
        // The centre pivot: a point already at local (0.5, 0.5, 0.5) is the
        // rotation axis itself and must map to the block's own centre exactly,
        // not the floor pivot chest's matrix uses.
        let centre = m.transform_point3(Vec3::splat(0.5));
        assert!(
            (centre - origin_centre).length() < 1e-5,
            "expected the centre point fixed at the block's own centre {origin_centre}, got {centre}"
        );
    }

    // --- conduit ---------------------------------------------------------

    /// Every offset the 42-cell "plus ring" candidate set contains — computed
    /// once here from the exact predicate `conduit_frame_scan` uses, so the
    /// fixture-building tests below can place blocks at a *chosen subset* of
    /// real candidates rather than guessing coordinates that might not
    /// qualify at all.
    fn conduit_frame_candidates() -> Vec<[i32; 3]> {
        let mut out = Vec::new();
        for ox in -2i32..=2 {
            for oy in -2i32..=2 {
                for oz in -2i32..=2 {
                    let (ax, ay, az) = (ox.abs(), oy.abs(), oz.abs());
                    let outside_inner = ax > 1 || ay > 1 || az > 1;
                    let on_plus_ring = (ox == 0 && (ay == 2 || az == 2))
                        || (oy == 0 && (ax == 2 || az == 2))
                        || (oz == 0 && (ax == 2 || ay == 2));
                    if outside_inner && on_plus_ring {
                        out.push([ox, oy, oz]);
                    }
                }
            }
        }
        out
    }

    /// Outside-arithmetic control on the scan's geometry alone, independent of
    /// which blocks are placed: exactly [`CONDUIT_FRAME_CANDIDATE_COUNT`] cells
    /// qualify, matching vanilla's own "hunting" threshold — "hunting" is a
    /// full house, not a majority.
    #[test]
    fn conduit_frame_candidate_count_is_42_and_matches_min_kill_size() {
        let candidates = conduit_frame_candidates();
        assert_eq!(candidates.len(), CONDUIT_FRAME_CANDIDATE_COUNT as usize);
        let frame = conduit_frame_scan([0, 0, 0], |_| true, |_| true);
        assert_eq!(frame.effect_block_count, CONDUIT_FRAME_CANDIDATE_COUNT);
        assert!(frame.is_active());
        assert!(frame.is_hunting());
    }

    /// The conjunction trap: a room built entirely of frame blocks (all 42
    /// candidates present) is still an **empty** frame if even one cell of the
    /// conduit's own inner 3×3×3 is not water — vanilla's own shape update
    /// returns before the 5×5×5 pass ever runs. A resolver that scored the
    /// outer ring independently of the inner-cube gate would activate here;
    /// vanilla does not.
    #[test]
    fn conduit_frame_scan_requires_the_entire_inner_cube_to_be_water_even_with_every_frame_block_present()
     {
        let pos = [10, 20, 30];
        let missing = [pos[0], pos[1] + 1, pos[2]]; // one inner-cube cell, not water
        let frame = conduit_frame_scan(
            pos,
            |p| p != missing,
            |_| true, // every 5x5x5 candidate is a valid frame block
        );
        assert_eq!(
            frame.effect_block_count, 0,
            "one non-water inner cell must zero the whole frame even though \
             every outer candidate is a valid block"
        );
        assert!(!frame.is_active());
        assert!(!frame.is_hunting());
    }

    /// The discriminating pair for activation: 15 valid frame blocks (one
    /// short) against exactly 16 — `MIN_ACTIVE_SIZE`. Never "a bare conduit",
    /// which cannot tell "zero" from "any number below the threshold".
    #[test]
    fn conduit_frame_scan_discriminates_15_from_16_valid_blocks() {
        let pos = [0, 0, 0];
        let candidates = conduit_frame_candidates();
        for count in [15usize, 16usize] {
            let placed: std::collections::HashSet<[i32; 3]> = candidates[..count]
                .iter()
                .map(|o| [pos[0] + o[0], pos[1] + o[1], pos[2] + o[2]])
                .collect();
            let frame = conduit_frame_scan(pos, |_| true, |p| placed.contains(&p));
            assert_eq!(frame.effect_block_count, count as u32);
            assert_eq!(
                frame.is_active(),
                count >= 16,
                "count {count} active-ness mismatch"
            );
        }
    }

    /// The discriminating pair for hunting: 41 (active, not hunting) against
    /// exactly 42 — every candidate filled.
    #[test]
    fn conduit_frame_scan_requires_all_42_candidates_to_hunt() {
        let pos = [0, 0, 0];
        let candidates = conduit_frame_candidates();
        for count in [41usize, 42usize] {
            let placed: std::collections::HashSet<[i32; 3]> = candidates[..count]
                .iter()
                .map(|o| [pos[0] + o[0], pos[1] + o[1], pos[2] + o[2]])
                .collect();
            let frame = conduit_frame_scan(pos, |_| true, |p| placed.contains(&p));
            assert!(frame.is_active(), "{count} blocks must already be active");
            assert_eq!(
                frame.is_hunting(),
                count >= 42,
                "count {count} hunting mismatch"
            );
        }
    }

    /// `conduit_advance`'s two independent clauses: `tick_count` always steps,
    /// `active_rotation_ticks` only while active. Collected across a short
    /// active/inactive/active sequence and asserted as a table, not one
    /// `assert!` per iteration.
    #[test]
    fn conduit_advance_ticks_always_and_gates_rotation_on_active() {
        let steps = [true, true, false, false, true];
        let mut tick_count = 0u32;
        let mut active_rotation_ticks = 0u32;
        let mut rows = Vec::new();
        for active in steps {
            (tick_count, active_rotation_ticks) =
                conduit_advance(tick_count, active_rotation_ticks, active);
            rows.push((tick_count, active_rotation_ticks));
        }
        assert_eq!(
            rows,
            vec![(1, 1), (2, 2), (3, 2), (4, 2), (5, 3)],
            "tick_count must step every call; active_rotation_ticks only on an \
             active call"
        );
    }

    /// The unit trap, predicted exactly rather than sign-checked: the same
    /// `conduit_active_rotation_value` output is degrees in
    /// [`conduit_inactive_y_rot_radians`] and radians in
    /// [`conduit_active_axis_rotation_radians`] — a ~57× (`180/π`) gap between
    /// the two readings of the identical number. Both hypotheses computed from
    /// outside arithmetic (`f32::to_radians`, and the identity), not from a
    /// remembered literal.
    #[test]
    fn conduit_active_rotation_value_is_degrees_when_inactive_and_radians_when_active() {
        let active_ticks = 10u32;
        let partial = 0.5f32;

        let active_value = conduit_active_rotation_value(active_ticks, partial, true);
        let expected_active = (active_ticks as f32 + partial) * -0.0375;
        assert!((active_value - expected_active).abs() < 1e-6);
        // Active branch: used directly as radians.
        let active_axis_rad = conduit_active_axis_rotation_radians(active_value);
        assert!((active_axis_rad - expected_active).abs() < 1e-6);

        let inactive_value = conduit_active_rotation_value(active_ticks, partial, false);
        let expected_inactive = active_ticks as f32 * -0.0375;
        assert!(
            (inactive_value - expected_inactive).abs() < 1e-6,
            "inactive reading must drop the partial tick entirely"
        );
        // Inactive branch: the same *shape* of number, but read as degrees.
        let inactive_y_rot_rad = conduit_inactive_y_rot_radians(inactive_value);
        let expected_inactive_rad = expected_inactive.to_radians();
        assert!((inactive_y_rot_rad - expected_inactive_rad).abs() < 1e-6);

        // Isolate the two *readings* of one identical number (zero partial
        // tick, so the active branch's counter and the inactive branch's
        // counter coincide at `10`): applying `conduit_active_axis_rotation_radians`
        // (identity) against `conduit_inactive_y_rot_radians` (`to_radians`)
        // to the same value must differ by exactly `180/pi` — treating the
        // two readings as interchangeable is the exact bug this exists to
        // catch.
        let same_value = conduit_active_rotation_value(active_ticks, 0.0, true);
        assert!(
            (same_value - conduit_active_rotation_value(active_ticks, 0.0, false)).abs() < 1e-6,
            "zero partial tick must make the two branches' counters coincide"
        );
        let as_radians = conduit_active_axis_rotation_radians(same_value).abs();
        let as_degrees_then_radians = conduit_inactive_y_rot_radians(same_value).abs();
        let ratio = as_radians / as_degrees_then_radians;
        assert!(
            (ratio - 180.0 / std::f32::consts::PI).abs() < 1e-3,
            "expected the two readings of the same value to differ by exactly \
             180/pi (~57.2958x), got {ratio}x"
        );
    }

    /// `conduit_bob`'s exact formula at two discriminating `anim_time`s — `0`
    /// (`sin == 0`) and `5π` (`sin(0.5π) == 1`) — never a round guess.
    #[test]
    fn conduit_bob_matches_the_exact_formula_at_discriminating_anim_times() {
        let at_zero = conduit_bob(0.0);
        assert!(
            (at_zero - 0.75).abs() < 1e-5,
            "sin(0)=0 -> hh=0.5 -> hh*hh+hh=0.75, got {at_zero}"
        );
        let at_peak = conduit_bob(5.0 * std::f32::consts::PI);
        assert!(
            (at_peak - 2.0).abs() < 1e-4,
            "sin(pi/2)=1 -> hh=1.0 -> hh*hh+hh=2.0, got {at_peak}"
        );
    }

    /// `tickCount / 66 % 3` steps on **integer** ticks only — checked either
    /// side of both seams (`66`, `132`, `198`), the discriminating boundary
    /// inputs rather than round numbers.
    #[test]
    fn conduit_animation_phase_steps_every_66_ticks() {
        let cases = [
            (0u32, 0u8),
            (65, 0),
            (66, 1),
            (131, 1),
            (132, 2),
            (197, 2),
            (198, 0),
        ];
        let mismatches: Vec<_> = cases
            .into_iter()
            .filter_map(|(tick, expected)| {
                let got = conduit_animation_phase(tick);
                (got != expected).then_some((tick, expected, got))
            })
            .collect();
        assert!(mismatches.is_empty(), "{mismatches:?}");
    }

    /// The inactive branch resolves to exactly one instance — the shell —
    /// carrying the "degrees" reading of `active_rotation_value` threaded into
    /// its placement, matching [`conduit_inactive_y_rot_radians`] exactly
    /// (that function's own test pins down its arithmetic; this proves
    /// `resolve_conduit` actually plugs it in rather than the raw value).
    #[test]
    fn resolve_conduit_inactive_resolves_one_shell_instance_using_the_degrees_reading() {
        let models = BlockEntityModelSet::load();
        let spawn = ConduitSpawn {
            active_rotation_value: -30.0,
            ..ConduitSpawn::at([5, 10, -3])
        };
        let out = models.resolve_conduit(&spawn, Mat4::IDENTITY);
        assert_eq!(out.len(), 1, "inactive must resolve to exactly one instance");
        let inst = &out[0];
        assert_eq!(inst.model, CONDUIT_SHELL);
        assert_eq!(inst.texture, CONDUIT_SHELL_TEXTURE_STEM);

        let expected = Mat4::from_translation(Vec3::new(5.5, 10.5, -2.5))
            * Mat4::from_rotation_y(conduit_inactive_y_rot_radians(spawn.active_rotation_value));
        for (a, b) in inst.transform.to_cols_array().iter().zip(expected.to_cols_array()) {
            assert!((a - b).abs() < 1e-4, "{:?} != {:?}", inst.transform, expected);
        }
    }

    /// The active branch resolves to exactly four instances — cage, both wind
    /// planes, and the eye — with the right `(model, texture)` pairs, and the
    /// cage/eye share the bob height while **both** wind planes stay fixed at
    /// `y = 0.5`: the adjacent-same-shaped-expression trap this module's own
    /// doc comment calls out, checked with `hh != 1` so the two Y values
    /// cannot coincide by accident.
    #[test]
    fn resolve_conduit_active_resolves_cage_wind_wind_eye_with_the_bob_on_only_two_of_them() {
        let models = BlockEntityModelSet::load();
        // `anim_time` chosen so `conduit_bob` is not `1.0` (which would make
        // `bob == 0.5`, coincident with the wind planes' fixed value and
        // unable to distinguish the two).
        let anim_time = 1.0;
        let hh = conduit_bob(anim_time);
        let bob = 0.3 + hh * 0.2;
        assert!(
            (bob - 0.5).abs() > 1e-3,
            "bob {bob} must differ from the wind planes' fixed 0.5 for this test to discriminate"
        );
        let spawn = ConduitSpawn {
            active: true,
            hunting: false,
            active_rotation_value: 0.0,
            anim_time,
            animation_phase: 0,
            ..ConduitSpawn::at([0, 0, 0])
        };
        let out = models.resolve_conduit(&spawn, Mat4::IDENTITY);
        assert_eq!(out.len(), 4, "active must resolve to exactly four instances");

        let mut by_model: std::collections::HashMap<&str, Vec<&BlockEntityInstance>> =
            std::collections::HashMap::new();
        for inst in &out {
            by_model.entry(inst.model).or_default().push(inst);
        }
        assert_eq!(by_model.get(CONDUIT_CAGE).map(Vec::len), Some(1));
        assert_eq!(by_model.get(CONDUIT_WIND).map(Vec::len), Some(2));
        assert_eq!(by_model.get(CONDUIT_EYE).map(Vec::len), Some(1));

        let cage = by_model[CONDUIT_CAGE][0];
        assert_eq!(cage.texture, CONDUIT_CAGE_TEXTURE_STEM);
        let eye = by_model[CONDUIT_EYE][0];
        assert_eq!(eye.texture, CONDUIT_CLOSED_EYE_TEXTURE_STEM);

        let y_of = |inst: &BlockEntityInstance| inst.transform.col(3).y;
        assert!(
            (y_of(cage) - bob).abs() < 1e-4,
            "cage Y {} != bob {bob}",
            y_of(cage)
        );
        assert!(
            (y_of(eye) - bob).abs() < 1e-4,
            "eye Y {} != bob {bob}",
            y_of(eye)
        );
        for wind in &by_model[CONDUIT_WIND] {
            assert!(
                (y_of(wind) - 0.5).abs() < 1e-4,
                "wind plane Y {} must stay fixed at 0.5, not the bob {bob}",
                y_of(wind)
            );
        }
        // The two wind instances share a texture but must not share a
        // transform (the second carries the extra `scale(0.875)` +
        // `rotationXYZ` clause).
        assert_ne!(
            by_model[CONDUIT_WIND][0].transform, by_model[CONDUIT_WIND][1].transform,
            "the two wind planes must be posed differently"
        );
    }

    /// Both wind planes switch texture together with `animation_phase == 1`,
    /// and only then.
    #[test]
    fn resolve_conduit_wind_texture_follows_animation_phase() {
        let models = BlockEntityModelSet::load();
        for (phase, expected) in [
            (0u8, CONDUIT_WIND_TEXTURE_STEM),
            (1u8, CONDUIT_WIND_VERTICAL_TEXTURE_STEM),
            (2u8, CONDUIT_WIND_TEXTURE_STEM),
        ] {
            let spawn = ConduitSpawn {
                active: true,
                animation_phase: phase,
                ..ConduitSpawn::at([0, 0, 0])
            };
            let out = models.resolve_conduit(&spawn, Mat4::IDENTITY);
            let winds: Vec<_> = out.iter().filter(|i| i.model == CONDUIT_WIND).collect();
            assert_eq!(winds.len(), 2);
            for w in winds {
                assert_eq!(w.texture, expected, "phase {phase}");
            }
        }
    }

    /// The eye's sprite follows `hunting`, independent of everything else
    /// about the spawn.
    #[test]
    fn resolve_conduit_eye_texture_follows_hunting() {
        let models = BlockEntityModelSet::load();
        for (hunting, expected) in [
            (false, CONDUIT_CLOSED_EYE_TEXTURE_STEM),
            (true, CONDUIT_OPEN_EYE_TEXTURE_STEM),
        ] {
            let spawn = ConduitSpawn {
                active: true,
                hunting,
                ..ConduitSpawn::at([0, 0, 0])
            };
            let out = models.resolve_conduit(&spawn, Mat4::IDENTITY);
            let eye = out.iter().find(|i| i.model == CONDUIT_EYE).expect("eye");
            assert_eq!(eye.texture, expected, "hunting {hunting}");
        }
    }

    /// Only the eye reads `camera_orientation` — vanilla's own
    /// pose-stack multiply by the camera orientation sits inside the eye's own
    /// push/pop pose pair alone. Changing it must move the eye's
    /// transform and leave the cage's and both wind planes' untouched.
    #[test]
    fn resolve_conduit_only_the_eye_billboards_with_camera_orientation() {
        let models = BlockEntityModelSet::load();
        let spawn = ConduitSpawn {
            active: true,
            ..ConduitSpawn::at([0, 0, 0])
        };
        let identity = models.resolve_conduit(&spawn, Mat4::IDENTITY);
        let rotated = models.resolve_conduit(&spawn, Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2));

        let by_model = |out: &[BlockEntityInstance], model: &str| -> Vec<Mat4> {
            out.iter().filter(|i| i.model == model).map(|i| i.transform).collect()
        };
        assert_eq!(by_model(&identity, CONDUIT_CAGE), by_model(&rotated, CONDUIT_CAGE));
        assert_eq!(by_model(&identity, CONDUIT_WIND), by_model(&rotated, CONDUIT_WIND));
        assert_ne!(
            by_model(&identity, CONDUIT_EYE),
            by_model(&rotated, CONDUIT_EYE),
            "the eye must be the one instance that moves with camera orientation"
        );
    }

    /// [`conduit_texture_stems`] and [`block_entity_texture_stems`] both carry
    /// all six conduit sheets — the loader's own enumeration, checked as a set
    /// rather than assuming the union function forwards correctly (the
    /// `Arc<S>`-forwarding class of bug this repo has shipped before: adding a
    /// stem list without adding it to the aggregator compiles clean and stays
    /// silently short).
    #[test]
    fn conduit_texture_stems_are_all_six_and_reach_the_aggregate_loader_list() {
        let stems = conduit_texture_stems();
        assert_eq!(stems.len(), 6, "{stems:?}");
        let all = block_entity_texture_stems();
        let missing: Vec<_> = stems.iter().filter(|s| !all.contains(s)).collect();
        assert!(
            missing.is_empty(),
            "conduit_texture_stems entries missing from block_entity_texture_stems: {missing:?}"
        );
    }
}
