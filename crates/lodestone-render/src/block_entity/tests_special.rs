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
