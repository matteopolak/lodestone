//! HUD state, title/action-bar timing, boss bar, advancement and statistics tests.

use lodestone_game::bossbar::{BossBar, BossBarColor, BossBarOverlay, BossBarSet};
use lodestone_game::player_state::{ActionBar, HotbarSlot, HudState, TitlePhase, TitleState, TitleTimes};
use lodestone_game::progress::{
    AddedAdvancement, AdvancementProgress, Advancements, AdvancementsUpdate, StatKey, Statistics,
};
use lodestone_model::{GameMode, Text};
use uuid::Uuid;

fn id(s: &str) -> lodestone_model::Identifier {
    s.parse().unwrap()
}

#[test]
fn set_health_marks_dead_and_respawn_clears() {
    let mut hud = HudState::new();
    assert!(!hud.is_dead());
    hud.set_health(0.0, 20, 5.0);
    assert!(hud.is_dead());
    hud.respawn();
    assert!(!hud.is_dead());
    assert_eq!(hud.health, 20.0);
}

#[test]
fn game_mode_change_records_previous() {
    let mut hud = HudState::new();
    hud.set_game_mode(GameMode::Creative);
    assert_eq!(hud.game_mode, GameMode::Creative);
    assert_eq!(hud.previous_game_mode, Some(GameMode::Survival));
    // Setting the same mode again is a no-op for previous.
    hud.set_game_mode(GameMode::Creative);
    assert_eq!(hud.previous_game_mode, Some(GameMode::Survival));
}

#[test]
fn select_slot_rejects_out_of_range() {
    let mut hud = HudState::new();
    let last = HotbarSlot::new(8).expect("the last hotbar slot is valid");
    hud.select_slot(last);
    assert_eq!(hud.selected_slot, last);
    assert_eq!(HotbarSlot::new(9), None, "there is no tenth hotbar slot");
}

#[test]
fn title_phases_follow_default_timing() {
    // Defaults 10/70/20 => total 100.
    let mut t = TitleState::new();
    t.set_title(Text::literal("Chapter I"));
    assert_eq!(t.phase(), TitlePhase::FadeIn);
    assert!((t.alpha() - 0.0).abs() < 1e-6);

    t.tick(5); // mid fade-in
    assert_eq!(t.phase(), TitlePhase::FadeIn);
    assert!((t.alpha() - 0.5).abs() < 1e-6);

    t.tick(5); // elapsed 10 -> stay
    assert_eq!(t.phase(), TitlePhase::Stay);
    assert!((t.alpha() - 1.0).abs() < 1e-6);

    t.tick(70); // elapsed 80 -> fade-out begins
    assert_eq!(t.phase(), TitlePhase::FadeOut);
    assert!((t.alpha() - 1.0).abs() < 1e-6);

    t.tick(10); // elapsed 90, 10 ticks left of 20 fade-out
    assert!((t.alpha() - 0.5).abs() < 1e-6);

    t.tick(10); // elapsed 100 -> done, cleared
    assert_eq!(t.phase(), TitlePhase::Done);
    assert!(t.title().is_none());
}

#[test]
fn setting_subtitle_restarts_timer_when_title_shown() {
    let mut t = TitleState::new();
    t.set_title(Text::literal("Title"));
    t.tick(50);
    assert_eq!(t.phase(), TitlePhase::Stay);
    t.set_subtitle(Text::literal("sub"));
    // Timer restarts -> back to fade-in.
    assert_eq!(t.phase(), TitlePhase::FadeIn);
    assert_eq!(t.subtitle().unwrap().to_plain_string(), "sub");
}

#[test]
fn custom_times_with_zero_fades_do_not_divide_by_zero() {
    let mut t = TitleState::new();
    t.set_times(TitleTimes {
        fade_in: 0,
        stay: 40,
        fade_out: 0,
    });
    t.set_title(Text::literal("instant"));
    assert!((t.alpha() - 1.0).abs() < 1e-6);
    assert_eq!(t.phase(), TitlePhase::Stay);
}

#[test]
fn action_bar_shows_then_fades_and_expires() {
    let mut ab = ActionBar::new();
    ab.set(Text::literal("hint"));
    assert!((ab.alpha() - 1.0).abs() < 1e-6);
    ab.tick(50); // remaining 10 -> still full
    assert!((ab.alpha() - 1.0).abs() < 1e-6);
    ab.tick(5); // remaining 5 -> fading
    assert!((ab.alpha() - 0.5).abs() < 1e-6);
    ab.tick(5); // remaining 0 -> gone
    assert!(ab.text().is_none());
}

#[test]
fn boss_bar_set_add_update_remove_and_flags() {
    let mut set = BossBarSet::new();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let mut bar = BossBar::new(Text::literal("Ender Dragon"));
    bar.color = BossBarColor::Purple;
    bar.overlay = BossBarOverlay::Notched10;
    bar.create_fog = true;
    set.add(a, bar);
    set.add(b, BossBar::new(Text::literal("Wither")));
    assert_eq!(set.len(), 2);
    assert!(set.any_fog());
    assert!(!set.any_darken_screen());

    set.get_mut(&a).unwrap().set_progress(2.0); // clamps to 1.0
    assert!((set.get(&a).unwrap().progress - 1.0).abs() < 1e-6);
    set.get_mut(&a).unwrap().set_progress(-1.0);
    assert!((set.get(&a).unwrap().progress - 0.0).abs() < 1e-6);

    // Insertion order preserved.
    let order: Vec<Uuid> = set.iter().map(|(id, _)| *id).collect();
    assert_eq!(order, [a, b]);

    set.remove(&a);
    assert_eq!(set.len(), 1);
    assert!(!set.any_fog());
}

#[test]
fn boss_bar_enum_ids_round_trip() {
    for i in 0u8..=6 {
        assert_eq!(BossBarColor::from_id(i).unwrap().id(), i);
    }
    for i in 0u8..=4 {
        assert_eq!(BossBarOverlay::from_id(i).unwrap().id(), i);
    }
    assert!(BossBarColor::from_id(7).is_none());
    assert!(BossBarOverlay::from_id(5).is_none());
}

#[test]
fn advancement_done_when_all_criteria_obtained() {
    let mut p = AdvancementProgress::from_criteria(["got_wood", "made_table"]);
    assert!(!p.is_done());
    assert!((p.fraction() - 0.0).abs() < 1e-6);
    p.obtain("got_wood", 1000);
    assert!((p.fraction() - 0.5).abs() < 1e-6);
    assert!(!p.is_done());
    p.obtain("made_table", 2000);
    assert!(p.is_done());
    p.revoke("got_wood");
    assert!(!p.is_done());
}

#[test]
fn advancements_store_tracks_completion() {
    let mut adv = Advancements::new();
    let mut done = AdvancementProgress::from_criteria(["a"]);
    done.obtain("a", 1);
    adv.set(id("minecraft:story/root"), done);
    adv.set(
        id("minecraft:story/mine_stone"),
        AdvancementProgress::from_criteria(["get_stone"]),
    );
    let completed: Vec<_> = adv.completed().cloned().collect();
    assert_eq!(completed, [id("minecraft:story/root")]);
}

#[test]
fn statistics_set_and_increment() {
    let mut stats = Statistics::new();
    let jumps = StatKey::new(id("minecraft:custom"), id("minecraft:jump"));
    assert_eq!(stats.get(&jumps), 0);
    stats.set(jumps.clone(), 5);
    stats.increment(jumps.clone(), 2);
    assert_eq!(stats.get(&jumps), 7);
}

#[test]
fn advancements_apply_adds_defs_then_progress() {
    let mut adv = Advancements::new();
    let root = id("minecraft:story/root");
    let update = AdvancementsUpdate {
        reset: false,
        added: vec![AddedAdvancement {
            id: root.clone(),
            criteria: vec!["crafting_table".into(), "get_wood".into()],
        }],
        removed: vec![],
        progress: vec![(
            root.clone(),
            vec![
                ("get_wood".into(), Some(1_000)),
                ("crafting_table".into(), None),
            ],
        )],
    };
    let unknown = adv.apply(update);
    assert!(unknown.is_empty());
    let p = adv.get(&root).unwrap();
    assert!(p.is_obtained("get_wood"));
    assert!(!p.is_obtained("crafting_table"));
    assert!(!p.is_done());
}

#[test]
fn advancements_apply_progress_for_unknown_is_reported() {
    let mut adv = Advancements::new();
    let ghost = id("minecraft:nope");
    let update = AdvancementsUpdate {
        progress: vec![(ghost.clone(), vec![("x".into(), Some(1))])],
        ..Default::default()
    };
    let unknown = adv.apply(update);
    assert_eq!(unknown, vec![ghost]);
    assert!(adv.is_empty());
}

#[test]
fn advancements_apply_reset_clears_before_applying() {
    let mut adv = Advancements::new();
    adv.set(
        id("minecraft:old"),
        AdvancementProgress::from_criteria(["c"]),
    );
    let new_id = id("minecraft:new");
    let update = AdvancementsUpdate {
        reset: true,
        added: vec![AddedAdvancement {
            id: new_id.clone(),
            criteria: vec!["c".into()],
        }],
        progress: vec![(new_id.clone(), vec![("c".into(), Some(42))])],
        ..Default::default()
    };
    adv.apply(update);
    assert_eq!(adv.len(), 1);
    assert!(adv.get(&id("minecraft:old")).is_none());
    assert!(adv.get(&new_id).unwrap().is_done());
}

#[test]
fn advancements_apply_incremental_progress_without_readding() {
    let mut adv = Advancements::new();
    let root = id("minecraft:story/root");
    adv.apply(AdvancementsUpdate {
        added: vec![AddedAdvancement {
            id: root.clone(),
            criteria: vec!["a".into(), "b".into()],
        }],
        ..Default::default()
    });
    // A later packet with no `added`, just more progress, must still land.
    adv.apply(AdvancementsUpdate {
        progress: vec![(
            root.clone(),
            vec![("a".into(), Some(1)), ("b".into(), Some(2))],
        )],
        ..Default::default()
    });
    assert!(adv.get(&root).unwrap().is_done());
}

#[test]
fn advancements_apply_removed_drops_entry() {
    let mut adv = Advancements::new();
    let gone = id("minecraft:gone");
    adv.set(gone.clone(), AdvancementProgress::from_criteria(["c"]));
    adv.apply(AdvancementsUpdate {
        removed: vec![gone.clone()],
        ..Default::default()
    });
    assert!(adv.get(&gone).is_none());
}

#[test]
fn statistics_apply_sets_absolute_totals() {
    let mut stats = Statistics::new();
    let jumps = StatKey::new(id("minecraft:custom"), id("minecraft:jump"));
    let walk = StatKey::new(id("minecraft:custom"), id("minecraft:walk_one_cm"));
    stats.increment(jumps.clone(), 99);
    // award_stats carries absolute values; apply overwrites, not adds.
    stats.apply([(jumps.clone(), 5), (walk.clone(), 1_234)]);
    assert_eq!(stats.get(&jumps), 5);
    assert_eq!(stats.get(&walk), 1_234);
    assert_eq!(stats.len(), 2);
}
