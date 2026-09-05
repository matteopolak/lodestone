//! Hermetic tests for the HUD read-model ([`lodestone_game::hud`]) and the
//! active-effects subsystem — proving one coherent snapshot assembles correctly
//! from the individual subsystem states.

use lodestone_game::bossbar::{BossBar, BossBarSet};
use lodestone_game::effect::{ActiveEffects, StatusEffect};
use lodestone_game::hud::{HudInputs, HudSnapshot};
use lodestone_game::item::ItemStack;
use lodestone_game::menu::Menu;
use lodestone_game::player_state::{ActionBar, HotbarSlot, HudState, TitleState};
use lodestone_game::scoreboard::{DisplaySlot, Objective, Scoreboard};
use lodestone_game::tablist::{GameProfile, PlayerListEntry, TabList};
use lodestone_model::{GameMode, Text};
use uuid::Uuid;

fn id(s: &str) -> lodestone_model::Identifier {
    s.parse().unwrap()
}

#[test]
fn snapshot_reports_vitals_and_air() {
    let mut hud = HudState::new();
    hud.set_health(7.0, 12, 3.0);
    hud.set_air(120);
    hud.set_experience(0.4, 5, 55);

    let menu = Menu::player();
    let effects = ActiveEffects::new();
    let bars = BossBarSet::new();
    let sb = Scoreboard::new();
    let tab = TabList::new();
    let title = TitleState::new();
    let action = ActionBar::new();

    let inputs = HudInputs {
        hud: &hud,
        menu: &menu,
        effects: &effects,
        boss_bars: &bars,
        scoreboard: &sb,
        tab_list: &tab,
        title: &title,
        action_bar: &action,
    };
    let snap = HudSnapshot::assemble(&inputs);

    assert_eq!(snap.health, 7.0);
    assert_eq!(snap.food, 12);
    assert_eq!(snap.air, 120);
    assert_eq!(snap.max_air, HudState::MAX_AIR);
    assert_eq!(snap.xp_level, 5);
    assert!((snap.xp_progress - 0.4).abs() < f32::EPSILON);
    assert!(!snap.dead);
}

#[test]
fn air_clamps_and_defaults_full() {
    let mut hud = HudState::new();
    assert_eq!(hud.air, HudState::MAX_AIR, "air defaults to full");
    hud.set_air(9_999);
    assert_eq!(hud.air, HudState::MAX_AIR, "air clamps to max");
    hud.set_air(-5);
    assert_eq!(hud.air, 0, "air clamps to zero");
    hud.set_air(50);
    hud.respawn();
    assert_eq!(hud.air, HudState::MAX_AIR, "respawn refills air");
}

#[test]
fn hotbar_reflects_selected_slot_and_contents() {
    let mut menu = Menu::player();
    // native hotbar slots 0..=8 map to menu slots 36..=44.
    menu.set_slot_item(36, Some(ItemStack::new(id("minecraft:stone"), 64)));
    menu.set_slot_item(40, Some(ItemStack::new(id("minecraft:torch"), 12)));

    let mut hud = HudState::new();
    let selected = HotbarSlot::new(4).expect("native hotbar index 4 is valid");
    hud.select_slot(selected); // native hotbar index 4 == menu slot 40

    let effects = ActiveEffects::new();
    let bars = BossBarSet::new();
    let sb = Scoreboard::new();
    let tab = TabList::new();
    let title = TitleState::new();
    let action = ActionBar::new();
    let inputs = HudInputs {
        hud: &hud,
        menu: &menu,
        effects: &effects,
        boss_bars: &bars,
        scoreboard: &sb,
        tab_list: &tab,
        title: &title,
        action_bar: &action,
    };
    let snap = HudSnapshot::assemble(&inputs);

    assert_eq!(snap.hotbar.selected, selected);
    assert_eq!(
        snap.hotbar.slots[0].unwrap().item().to_string(),
        "minecraft:stone"
    );
    assert_eq!(snap.hotbar.slots[0].unwrap().count(), 64);
    assert_eq!(
        snap.hotbar.held().unwrap().item().to_string(),
        "minecraft:torch"
    );
    assert!(snap.hotbar.slots[1].is_none());
}

#[test]
fn effects_and_bossbars_flow_through_in_order() {
    let mut effects = ActiveEffects::new();
    effects.apply(StatusEffect::new(id("minecraft:speed"), 0, 200));
    effects.apply(StatusEffect::new(id("minecraft:regeneration"), 1, 100));

    let mut bars = BossBarSet::new();
    bars.add(
        Uuid::from_u128(1),
        BossBar::new(Text::literal("Ender Dragon")),
    );
    bars.add(Uuid::from_u128(2), BossBar::new(Text::literal("Wither")));

    let hud = HudState::new();
    let menu = Menu::player();
    let sb = Scoreboard::new();
    let tab = TabList::new();
    let title = TitleState::new();
    let action = ActionBar::new();
    let inputs = HudInputs {
        hud: &hud,
        menu: &menu,
        effects: &effects,
        boss_bars: &bars,
        scoreboard: &sb,
        tab_list: &tab,
        title: &title,
        action_bar: &action,
    };
    let snap = HudSnapshot::assemble(&inputs);

    assert_eq!(snap.effects.len(), 2);
    assert_eq!(snap.effects[0].id.to_string(), "minecraft:speed");
    assert_eq!(snap.effects[1].level(), 2);
    assert_eq!(snap.boss_bars.len(), 2);
    assert_eq!(snap.boss_bars[0].title.to_plain_string(), "Ender Dragon");
}

#[test]
fn sidebar_shows_displayed_objective_sorted_and_capped() {
    let mut sb = Scoreboard::new();
    sb.add_objective(Objective::new("obj", "dummy", Text::literal("Stats")));
    // 20 holders; sidebar caps at 15, highest score first.
    for i in 0..20 {
        sb.set_score("obj", format!("p{i:02}"), i);
    }
    sb.set_display(DisplaySlot::Sidebar, Some("obj"));

    let hud = HudState::new();
    let menu = Menu::player();
    let effects = ActiveEffects::new();
    let bars = BossBarSet::new();
    let tab = TabList::new();
    let title = TitleState::new();
    let action = ActionBar::new();
    let inputs = HudInputs {
        hud: &hud,
        menu: &menu,
        effects: &effects,
        boss_bars: &bars,
        scoreboard: &sb,
        tab_list: &tab,
        title: &title,
        action_bar: &action,
    };
    let snap = HudSnapshot::assemble(&inputs);

    let sidebar = snap.sidebar.expect("sidebar objective displayed");
    assert_eq!(sidebar.title.to_plain_string(), "Stats");
    assert_eq!(sidebar.lines.len(), 15, "capped at 15 rows");
    // Highest score first.
    assert_eq!(sidebar.lines[0].value, 19);
    assert_eq!(sidebar.lines[14].value, 5);
}

#[test]
fn tablist_header_footer_and_listed_players() {
    let mut tab = TabList::new();
    tab.header = Some(Text::literal("Welcome"));
    tab.footer = Some(Text::literal("Bye"));
    let mut a = PlayerListEntry::new(GameProfile::new(Uuid::from_u128(10), "alice"));
    a.game_mode = GameMode::Creative;
    a.latency = 42;
    let mut hidden = PlayerListEntry::new(GameProfile::new(Uuid::from_u128(11), "ghost"));
    hidden.listed = false;
    tab.insert(a);
    tab.insert(hidden);

    let hud = HudState::new();
    let menu = Menu::player();
    let effects = ActiveEffects::new();
    let bars = BossBarSet::new();
    let sb = Scoreboard::new();
    let title = TitleState::new();
    let action = ActionBar::new();
    let inputs = HudInputs {
        hud: &hud,
        menu: &menu,
        effects: &effects,
        boss_bars: &bars,
        scoreboard: &sb,
        tab_list: &tab,
        title: &title,
        action_bar: &action,
    };
    let snap = HudSnapshot::assemble(&inputs);

    assert_eq!(snap.tab_header.unwrap().to_plain_string(), "Welcome");
    assert_eq!(snap.tab_footer.unwrap().to_plain_string(), "Bye");
    assert_eq!(snap.tab_players.len(), 1, "unlisted players are excluded");
    assert_eq!(snap.tab_players[0].profile.name, "alice");
    assert_eq!(snap.tab_players[0].latency, 42);
}

#[test]
fn title_and_actionbar_text_surface_with_alpha() {
    let mut title = TitleState::new();
    title.set_title(Text::literal("Chapter One"));
    let mut action = ActionBar::new();
    action.set(Text::literal("Objective updated"));

    let hud = HudState::new();
    let menu = Menu::player();
    let effects = ActiveEffects::new();
    let bars = BossBarSet::new();
    let sb = Scoreboard::new();
    let tab = TabList::new();
    let inputs = HudInputs {
        hud: &hud,
        menu: &menu,
        effects: &effects,
        boss_bars: &bars,
        scoreboard: &sb,
        tab_list: &tab,
        title: &title,
        action_bar: &action,
    };
    let snap = HudSnapshot::assemble(&inputs);

    assert_eq!(snap.title.unwrap().to_plain_string(), "Chapter One");
    assert_eq!(
        snap.action_bar.unwrap().to_plain_string(),
        "Objective updated"
    );
    assert!(snap.title_alpha >= 0.0 && snap.title_alpha <= 1.0);
    assert!(snap.action_bar_alpha >= 0.0 && snap.action_bar_alpha <= 1.0);
}
