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
