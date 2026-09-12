
use std::sync::Arc;

use uuid::Uuid;

use super::*;

    fn player() -> PermissionSubject {
        PermissionSubject::Player(Uuid::from_u128(1))
    }

    fn player_id() -> Uuid {
        Uuid::from_u128(1)
    }

    // ---------------------------------------------------------------------
    // Vanilla parity: PermissionLevel
    // ---------------------------------------------------------------------

    /// The five ids match vanilla's `PermissionLevel` enum exactly. Written as
    /// a table rather than five asserts so a reordering of the enum shows up
    /// as one failure naming the wrong pair.
    #[test]
    fn permission_level_ids_match_vanilla() {
        let table = [
            (PermissionLevel::All, 0, "all"),
            (PermissionLevel::Moderators, 1, "moderators"),
            (PermissionLevel::Gamemasters, 2, "gamemasters"),
            (PermissionLevel::Admins, 3, "admins"),
            (PermissionLevel::Owners, 4, "owners"),
        ];
        for (level, id, name) in table {
            assert_eq!(level.id(), id, "id for {level:?}");
            assert_eq!(level.serialized_name(), name, "name for {level:?}");
            assert_eq!(PermissionLevel::by_id(i32::from(id)), level, "by_id({id})");
        }
    }

    /// `by_id` clamps, because vanilla's own id-to-enum mapping clamps
    /// out-of-range ids rather than rejecting them. A hand-edited
    /// `ops.json` with `"level": 99` means OWNERS upstream, and must here.
    #[test]
    fn permission_level_by_id_clamps_out_of_range() {
        assert_eq!(PermissionLevel::by_id(99), PermissionLevel::Owners);
        assert_eq!(PermissionLevel::by_id(5), PermissionLevel::Owners);
        assert_eq!(PermissionLevel::by_id(-7), PermissionLevel::All);
    }

    /// The op boundary is level 1, not level 0 — `ops.json` cannot record a
    /// non-op, so "in the file" and ">= MODERATORS" are the same condition.
    #[test]
    fn only_level_one_and_up_counts_as_op() {
        assert!(!PermissionLevel::All.is_op());
        assert!(PermissionLevel::Moderators.is_op());
        assert!(PermissionLevel::Owners.is_op());
    }

    // ---------------------------------------------------------------------
    // Bukkit parity: PermissionDefault
    // ---------------------------------------------------------------------

    /// Bukkit's own default-to-boolean resolution table, all four
    /// values against both op states. Eight cells, because the interesting
    /// value (`NotOp`) is the one a three-value description of the default
    /// omits.
    #[test]
    fn permission_default_matches_bukkit_get_value_table() {
        let table = [
            (PermissionDefault::True, true, true),
            (PermissionDefault::True, false, true),
            (PermissionDefault::False, true, false),
            (PermissionDefault::False, false, false),
            (PermissionDefault::Op, true, true),
            (PermissionDefault::Op, false, false),
            (PermissionDefault::NotOp, true, false),
            (PermissionDefault::NotOp, false, true),
        ];
        for (default, op, expected) in table {
            assert_eq!(default.value(op), expected, "{default:?}.value({op})");
        }
    }

    /// Bukkit's own global default-permission fallback is op-only, so an undeclared node
    /// is held by an op and not by anyone else. This is the step most likely
    /// to be "corrected" to `False` by someone who has not read
    /// Bukkit's own permission-check algorithm, so it is pinned directly.
    #[test]
    fn an_undeclared_node_is_held_by_ops_and_nobody_else() {
        let mut permissions = Permissions::new();
        assert!(
            !permissions.has(player(), "nobody.declared.this"),
            "a non-op must not hold an undeclared node"
        );

        permissions
            .store
            .set_level(player_id(), PermissionLevel::Gamemasters);
        assert!(
            permissions.has(player(), "nobody.declared.this"),
            "an op must hold an undeclared node — Bukkit's global default-permission fallback is op-only"
        );
    }

    /// The negative control for the test above, and the reason
    /// [`Permissions::strict`] exists: with strict mode on, the *same* op no
    /// longer holds the *same* undeclared node. Without this pair, a bug that
    /// made `has` always return `true` for ops would look correct above.
    #[test]
    fn strict_mode_denies_an_undeclared_node_even_to_an_owner() {
        let mut permissions = Permissions::strict();
        permissions
            .store
            .set_level(player_id(), PermissionLevel::Owners);
        assert!(!permissions.has(player(), "nobody.declared.this"));

        // ... and strict mode changes *only* step 6: a declared `Op` node is
        // still held by the same owner, so the flag is narrow rather than a
        // blanket deny.
        permissions.declare("declared.node", PermissionDefault::Op);
        assert!(permissions.has(player(), "declared.node"));
    }

    /// A declared default is consulted before the undeclared fallback, and
    /// `True` really does reach a non-op.
    #[test]
    fn a_declared_true_default_reaches_a_non_op() {
        let mut permissions = Permissions::new();
        permissions.declare("myplugin.help", PermissionDefault::True);
        assert!(permissions.has(player(), "myplugin.help"));
        assert_eq!(permissions.level(player()), PermissionLevel::All);
    }

    /// `NotOp` inverts, which is the whole reason the fourth value exists.
    #[test]
    fn a_not_op_default_is_held_by_a_non_op_and_lost_when_opped() {
        let mut permissions = Permissions::new();
        permissions.declare("myplugin.hints", PermissionDefault::NotOp);
        assert!(permissions.has(player(), "myplugin.hints"));

        permissions
            .store
            .set_level(player_id(), PermissionLevel::Admins);
        assert!(!permissions.has(player(), "myplugin.hints"));
    }

    // ---------------------------------------------------------------------
    // Wildcards and specificity
    // ---------------------------------------------------------------------

    /// A wildcard grant covers the subtree *and* the bare prefix node, which
    /// is LuckPerms' behaviour and what `myplugin.*` is expected to mean.
    #[test]
    fn a_wildcard_grant_covers_the_subtree_and_the_bare_prefix() {
        let mut permissions = Permissions::new();
        permissions.declare("myplugin.admin.reload", PermissionDefault::Op);
        permissions.grant(player_id(), "myplugin.*");

        assert!(permissions.has(player(), "myplugin.admin.reload"));
        assert!(permissions.has(player(), "myplugin.anything.deeper.still"));
        assert!(permissions.has(player(), "myplugin"), "the bare prefix too");
    }

    /// The control for the wildcard test: a *neighbouring* tree is not
    /// covered. Without this, a `grant_matches` that returned `Some` for
    /// everything would pass the test above.
    #[test]
    fn a_wildcard_grant_does_not_leak_into_a_neighbouring_tree() {
        let mut permissions = Permissions::new();
        permissions.grant(player_id(), "myplugin.*");
        assert!(!permissions.has(player(), "otherplugin.admin"));
        // Nor a node that merely shares a textual prefix without a segment
        // boundary — `myplugin` must not match `myplugintwo`.
        assert!(!permissions.has(player(), "myplugintwo.admin"));
    }

    /// A more specific deny carves a hole in a broader allow — the
    /// `["myplugin.*", "-myplugin.admin"]` shape every real permissions
    /// config uses.
    #[test]
    fn a_specific_deny_carves_a_hole_in_a_wildcard_allow() {
        let mut permissions = Permissions::new();
        permissions.grant(player_id(), "myplugin.*");
        permissions.deny(player_id(), "myplugin.admin");

        assert!(permissions.has(player(), "myplugin.help"));
        assert!(!permissions.has(player(), "myplugin.admin"));

        // **The gotcha.** An *exact* deny does not cover its children: the only
        // key matching `myplugin.admin.reload` is the `myplugin.*` allow, so
        // the child is still permitted. To carve out a whole branch you must
        // deny the wildcard (`-myplugin.admin.*`), which is exactly LuckPerms'
        // behaviour and the mistake most permissions configs make once.
        assert!(
            permissions.has(player(), "myplugin.admin.reload"),
            "an exact deny must NOT cover children — only the wildcard form does"
        );

        permissions.deny(player_id(), "myplugin.admin.*");
        assert!(
            !permissions.has(player(), "myplugin.admin.reload"),
            "denying the wildcard form does cover the branch"
        );
    }

    /// Longer wildcards outrank shorter ones, and the bare `*` is the weakest
    /// grant there is.
    #[test]
    fn wildcard_specificity_is_ordered_by_literal_segment_count() {
        let mut permissions = Permissions::new();
        permissions.store.subject_mut(player_id()).grants =
            GrantSet::parse(["*", "-a.*", "a.b.*"]);

        assert!(permissions.has(player(), "z.anything"), "bare * allows");
        assert!(!permissions.has(player(), "a.other"), "-a.* is more specific");
        assert!(permissions.has(player(), "a.b.c"), "a.b.* is more specific still");
    }

    /// Step 4: within the same tier, a deny wins. Two **groups** are used
    /// deliberately — same tier, same specificity, opposite directions — since
    /// that is the only situation step 4 decides once step 3 has had its turn.
    /// Built by hand rather than through `grant`/`deny`, which would overwrite
    /// the same key in one set.
    #[test]
    fn a_deny_beats_an_allow_at_equal_specificity() {
        let mut permissions = Permissions::new();
        permissions.store.group_mut("staff").grants.deny("myplugin.admin");
        permissions.store.add_to_group(player_id(), "staff");
        permissions.store.group_mut("mods").grants.allow("myplugin.admin");
        permissions.store.add_to_group(player_id(), "mods");

        assert!(!permissions.has(player(), "myplugin.admin"));
    }

    // ---------------------------------------------------------------------
    // Groups
    // ---------------------------------------------------------------------

    /// A group grant reaches its members, and group inheritance is
    /// transitive.
    #[test]
    fn group_grants_are_inherited_transitively() {
        let mut permissions = Permissions::new();
        permissions.store.group_mut("owner").grants.allow("myplugin.reload");
        permissions.store.group_mut("admin").parents.push("owner".into());
        permissions.store.group_mut("staff").parents.push("admin".into());
        permissions.store.add_to_group(player_id(), "staff");

        assert!(
            permissions.has(player(), "myplugin.reload"),
            "staff -> admin -> owner must reach owner's grant"
        );
    }

    /// The control for inheritance: a player in no group does not get the
    /// grant. Without this, a `collect_grants` that scanned every group in the
    /// store regardless of membership would pass the test above.
    #[test]
    fn a_group_grant_does_not_reach_a_non_member() {
        let mut permissions = Permissions::new();
        permissions.store.group_mut("staff").grants.allow("myplugin.reload");
        permissions.declare("myplugin.reload", PermissionDefault::False);

        assert!(!permissions.has(player(), "myplugin.reload"));
    }

    /// A cyclic group graph terminates. The assertion is that the call
    /// *returns* — a missing visited set hangs or overflows the stack rather
    /// than returning a wrong answer, so this is a termination test, and the
    /// resolved value is checked too so it is not merely "it did not hang".
    #[test]
    fn cyclic_group_inheritance_terminates() {
        let mut permissions = Permissions::new();
        permissions.store.group_mut("a").parents.push("b".into());
        permissions.store.group_mut("b").parents.push("c".into());
        permissions.store.group_mut("c").parents.push("a".into());
        permissions.store.group_mut("c").grants.allow("deep.node");
        permissions.store.add_to_group(player_id(), "a");

        assert!(permissions.has(player(), "deep.node"));
    }

    /// A default group reaches a player nobody explicitly added — LuckPerms'
    /// `default` group.
    #[test]
    fn a_default_group_reaches_every_player() {
        let mut permissions = Permissions::new();
        permissions.declare("myplugin.basic", PermissionDefault::False);
        permissions.store.group_mut("default").grants.allow("myplugin.basic");
        permissions.store.add_default_group("default");

        assert!(permissions.has(player(), "myplugin.basic"));
    }

    /// The surprising consequence of comparing specificity *before*
    /// own-over-inherited, called out in the module doc's step 4 so nobody
    /// discovers it by accident: a group's **exact** allow beats the player's
    /// own **wildcard** deny.
    #[test]
    fn group_exact_grant_beats_player_wildcard_grant() {
        let mut permissions = Permissions::new();
        permissions.deny(player_id(), "myplugin.*");
        permissions.store.group_mut("staff").grants.allow("myplugin.admin");
        permissions.store.add_to_group(player_id(), "staff");

        assert!(
            permissions.has(player(), "myplugin.admin"),
            "exact beats wildcard regardless of which subject it came from"
        );
        assert!(
            !permissions.has(player(), "myplugin.other"),
            "and the player's own wildcard deny still covers everything else"
        );
    }

    /// Step 3: at equal specificity the subject's own grant outranks an
    /// inherited one, **including when the two disagree in direction**. This is
    /// the assertion that makes step 3 observable at all — see the module doc's
    /// step 4 for why an earlier ordering left it dead.
    #[test]
    fn player_grant_beats_group_grant_at_equal_specificity() {
        // Own deny vs inherited allow: the deny wins because it is the
        // player's own, not because it is a deny.
        let mut permissions = Permissions::new();
        permissions.deny(player_id(), "myplugin.admin");
        permissions.store.group_mut("staff").grants.allow("myplugin.admin");
        permissions.store.add_to_group(player_id(), "staff");
        assert!(!permissions.has(player(), "myplugin.admin"));

        // The mirror image, which is the half that distinguishes step 3 from
        // step 4: own **allow** vs inherited **deny** resolves to allow. If
        // negation were compared before tier, this would be `false`.
        let mut mirrored = Permissions::new();
        mirrored.grant(player_id(), "myplugin.admin");
        mirrored.store.group_mut("staff").grants.deny("myplugin.admin");
        mirrored.store.add_to_group(player_id(), "staff");
        assert!(
            mirrored.has(player(), "myplugin.admin"),
            "an own allow must beat an inherited deny — tier is compared before negation"
        );
    }

    // ---------------------------------------------------------------------
    // Case insensitivity
    // ---------------------------------------------------------------------

    /// Bukkit lowercases the node on both the set and the check side. Doing it
    /// on one side only is the classic half-case-insensitive bug, so both
    /// directions are exercised.
    #[test]
    fn nodes_are_case_insensitive_on_both_sides() {
        let mut permissions = Permissions::new();
        permissions.grant(player_id(), "MyPlugin.Admin");
        assert!(permissions.has(player(), "myplugin.admin"));

        let mut other = Permissions::new();
        other.grant(player_id(), "myplugin.admin");
        assert!(other.has(player(), "MYPLUGIN.ADMIN"));

        let mut declared = Permissions::new();
        declared.declare("MyPlugin.Help", PermissionDefault::True);
        assert!(declared.has(player(), "myplugin.help"));
    }

    // ---------------------------------------------------------------------
    // Levels through the resolution order
    // ---------------------------------------------------------------------

    /// A `HasCommandLevel` query is answered by the level, and is not affected
    /// by node grants — there is no node to match.
    #[test]
    fn a_level_query_is_answered_by_the_level_alone() {
        let mut permissions = Permissions::new();
        permissions
            .store
            .set_level(player_id(), PermissionLevel::Gamemasters);

        assert!(permissions.has_level(player(), PermissionLevel::Moderators));
        assert!(permissions.has_level(player(), PermissionLevel::Gamemasters));
        assert!(!permissions.has_level(player(), PermissionLevel::Admins));
    }

    /// The console holds everything, at every level — vanilla's
    /// `ALL_PERMISSIONS`.
    #[test]
    fn the_console_holds_everything() {
        let permissions = Permissions::new();
        assert!(permissions.has(PermissionSubject::Console, "anything.at.all"));
        assert!(permissions.has_level(PermissionSubject::Console, PermissionLevel::Owners));
        assert_eq!(
            permissions.level(PermissionSubject::Console),
            PermissionLevel::Owners
        );
    }

    /// Even an explicit deny does not stop the console, because step 6's
    /// short-circuit precedes grant matching. Pinned so the ordering is a
    /// decision rather than an accident.
    #[test]
    fn an_explicit_deny_does_not_stop_the_console() {
        let mut permissions = Permissions::new();
        // Deny it to *everyone* the only way the store can express.
        permissions.store.add_default_group("default");
        permissions.store.group_mut("default").grants.deny("*");
        assert!(permissions.has(PermissionSubject::Console, "anything"));
        assert!(!permissions.has(player(), "anything"));
    }

    // ---------------------------------------------------------------------
    // The resolver seam
    // ---------------------------------------------------------------------

    /// An installed resolver wins over everything, including an explicit deny
    /// and including the console short-circuit.
    #[test]
    fn an_installed_resolver_overrides_the_built_in_order() {
        let mut permissions = Permissions::new();
        permissions.deny(player_id(), "myplugin.admin");
        assert!(!permissions.has(player(), "myplugin.admin"));

        permissions.set_resolver(Arc::new(|_q: &PermissionQuery<'_>| Some(true)));
        assert!(
            permissions.has(player(), "myplugin.admin"),
            "a total-takeover resolver must beat an explicit deny"
        );
        assert!(permissions.has_resolver());

        // And it beats the console short-circuit in the other direction.
        permissions.set_resolver(Arc::new(|_q: &PermissionQuery<'_>| Some(false)));
        assert!(!permissions.has(PermissionSubject::Console, "anything"));
    }

    /// A resolver returning `None` falls through to the built-in order — the
    /// property that makes selective override possible. The control that the
    /// resolver was actually *consulted* is the counter: without it, a
    /// resolver that was never called would produce the same booleans.
    #[test]
    fn a_resolver_returning_none_falls_through_and_was_still_consulted() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);

        let mut permissions = Permissions::new();
        permissions.declare("myplugin.help", PermissionDefault::True);
        permissions.set_resolver(Arc::new(move |_q: &PermissionQuery<'_>| {
            seen.fetch_add(1, Ordering::SeqCst);
            None
        }));

        assert!(
            permissions.has(player(), "myplugin.help"),
            "fall-through must reach the declared True default"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the resolver must actually have been consulted"
        );
    }

    /// A resolver can decide *selectively*, using the query's own node and the
    /// registry/store it is handed.
    #[test]
    fn a_resolver_can_decide_only_its_own_nodes() {
        let mut permissions = Permissions::new();
        permissions.declare("other.node", PermissionDefault::True);
        permissions.set_resolver(Arc::new(|q: &PermissionQuery<'_>| {
            match q.node() {
                Some(node) if node.starts_with("luckperms.") => Some(true),
                _ => None,
            }
        }));

        assert!(permissions.has(player(), "luckperms.anything"));
        assert!(permissions.has(player(), "other.node"), "fell through");
        assert!(!permissions.has(player(), "undeclared.node"), "fell through to Op");
    }

    /// The query really carries the level and the subject, so a resolver can
    /// implement its own op logic rather than only pattern-matching nodes.
    #[test]
    fn the_query_carries_the_subject_and_its_level() {
        let mut permissions = Permissions::new();
        permissions
            .store
            .set_level(player_id(), PermissionLevel::Admins);
        permissions.set_resolver(Arc::new(|q: &PermissionQuery<'_>| {
            Some(q.level == PermissionLevel::Admins && matches!(q.subject, PermissionSubject::Player(_)))
        }));
        assert!(permissions.has(player(), "whatever"));
        assert!(!permissions.has(PermissionSubject::Console, "whatever"));
    }

    // ---------------------------------------------------------------------
    // Vanilla's level-based set, kept distinct on purpose
    // ---------------------------------------------------------------------

    /// Vanilla's `LevelBasedPermissionSet` answers a level check by level.
    #[test]
    fn vanilla_level_set_answers_a_level_check_by_level() {
        let set = LevelBasedPermissionSet::for_level(PermissionLevel::Gamemasters);
        assert!(set.has_permission(&Permission::HasCommandLevel(PermissionLevel::Moderators)));
        assert!(set.has_permission(&Permission::HasCommandLevel(PermissionLevel::Gamemasters)));
        assert!(!set.has_permission(&Permission::HasCommandLevel(PermissionLevel::Admins)));
    }

    /// Vanilla's one special-cased atom, both spellings.
    #[test]
    fn vanilla_level_set_special_cases_entity_selectors_at_gamemaster() {
        let gm = LevelBasedPermissionSet::for_level(PermissionLevel::Gamemasters);
        let mod_ = LevelBasedPermissionSet::for_level(PermissionLevel::Moderators);
        for node in [
            "commands/entity_selectors",
            "minecraft:commands/entity_selectors",
        ] {
            assert!(gm.has_permission(&Permission::atom(node)), "gm {node}");
            assert!(!mod_.has_permission(&Permission::atom(node)), "mod {node}");
        }
    }

    /// **The divergence, pinned.** Vanilla's level-based set denies an
    /// undeclared atom even to an owner; Bukkit's order (what [`Permissions`]
    /// implements) grants it to any op. Both behaviours are correct for their
    /// own upstream, and this test exists so nobody "fixes" one into the
    /// other without reading the module doc.
    #[test]
    fn vanilla_level_set_denies_an_undeclared_atom_where_bukkit_grants_it() {
        let owner_set = LevelBasedPermissionSet::for_level(PermissionLevel::Owners);
        assert!(
            !owner_set.has_permission(&Permission::atom("myplugin.admin")),
            "vanilla: every atom but entity_selectors is false at every level"
        );

        let mut permissions = Permissions::new();
        permissions
            .store
            .set_level(player_id(), PermissionLevel::Owners);
        assert!(
            permissions.has(player(), "myplugin.admin"),
            "Bukkit: an undeclared atom defaults to OP"
        );
    }

    /// `union` keeps the higher level.
    #[test]
    fn vanilla_level_set_union_keeps_the_higher_level() {
        let a = LevelBasedPermissionSet::for_level(PermissionLevel::Moderators);
        let b = LevelBasedPermissionSet::for_level(PermissionLevel::Admins);
        assert_eq!(a.union(b).level, PermissionLevel::Admins);
        assert_eq!(b.union(a).level, PermissionLevel::Admins);
    }

    // ---------------------------------------------------------------------
    // GrantSet text form
    // ---------------------------------------------------------------------

    /// LuckPerms' `-node` text form parses to a deny.
    #[test]
    fn grant_set_parses_the_luckperms_minus_prefix_as_a_deny() {
        let set = GrantSet::parse(["myplugin.*", "-myplugin.admin"]);
        let entries: Vec<_> = set.iter().collect();
        assert_eq!(
            entries,
            vec![
                ("myplugin.*", Grant::Allow),
                ("myplugin.admin", Grant::Deny),
            ]
        );
    }

    /// An empty store resolves nothing, so `best_match` really does return
    /// `None` rather than a default-shaped `Allow` — the precondition every
    /// step-5/6 test above depends on.
    #[test]
    fn an_empty_grant_set_matches_nothing() {
        let set = GrantSet::new();
        assert!(set.is_empty());
        assert!(set.best_match("anything", true).is_none());
    }
