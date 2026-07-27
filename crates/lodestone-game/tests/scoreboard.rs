//! Scoreboard, display-slot, and team tests.

use lodestone_game::scoreboard::{DisplaySlot, Objective, ScoreEntry, Scoreboard, Team, TeamColor};
use lodestone_model::Text;

fn obj(name: &str) -> Objective {
    Objective::new(name, "dummy", Text::literal(name))
}

#[test]
fn display_slot_ids_cover_all_nineteen() {
    for id in 0u8..=18 {
        let slot = DisplaySlot::from_id(id).expect("valid slot id");
        assert_eq!(slot.id(), id, "round-trip id {id}");
    }
    assert!(DisplaySlot::from_id(19).is_none());
    // Spot-check the coloured sidebar mapping against vanilla ids.
    assert_eq!(DisplaySlot::TeamSidebar(TeamColor::Black).id(), 3);
    assert_eq!(DisplaySlot::TeamSidebar(TeamColor::White).id(), 18);
    assert_eq!(DisplaySlot::TeamSidebar(TeamColor::Red).id(), 15);
}

#[test]
fn scores_sort_descending_then_by_name() {
    let mut sb = Scoreboard::new();
    sb.add_objective(obj("kills"));
    sb.set_score("kills", "alice", 5);
    sb.set_score("kills", "bob", 9);
    sb.set_score("kills", "carol", 5);
    let sorted: Vec<&str> = sb
        .sorted_scores("kills")
        .into_iter()
        .map(|(h, _)| h)
        .collect();
    // bob (9), then alice & carol tie at 5 -> alphabetical.
    assert_eq!(sorted, ["bob", "alice", "carol"]);
}

#[test]
fn removing_objective_clears_its_display_slot() {
    let mut sb = Scoreboard::new();
    sb.add_objective(obj("kills"));
    sb.set_display(DisplaySlot::Sidebar, Some("kills"));
    assert_eq!(sb.displayed(DisplaySlot::Sidebar), Some("kills"));
    sb.remove_objective("kills");
    assert_eq!(sb.displayed(DisplaySlot::Sidebar), None);
}

#[test]
fn coloured_sidebar_preferred_over_plain() {
    let mut sb = Scoreboard::new();
    sb.add_objective(obj("plain"));
    sb.add_objective(obj("reds"));
    sb.set_display(DisplaySlot::Sidebar, Some("plain"));
    sb.set_display(DisplaySlot::TeamSidebar(TeamColor::Red), Some("reds"));

    // A red-team player reads the coloured slot.
    assert_eq!(sb.sidebar_for_color(Some(TeamColor::Red)), Some("reds"));
    // A blue-team player (no blue slot set) falls back to the plain sidebar.
    assert_eq!(sb.sidebar_for_color(Some(TeamColor::Blue)), Some("plain"));
    // A teamless player uses the plain sidebar.
    assert_eq!(sb.sidebar_for_color(None), Some("plain"));
}

#[test]
fn team_membership_is_exclusive_and_reindexed() {
    let mut sb = Scoreboard::new();
    sb.add_team(Team::new("red"));
    sb.add_team(Team::new("blue"));
    assert!(sb.add_member("red", "steve"));
    assert_eq!(sb.team_of("steve").unwrap().name, "red");

    // Moving to blue removes from red.
    sb.add_member("blue", "steve");
    assert_eq!(sb.team_of("steve").unwrap().name, "blue");
    assert!(
        !sb.team("red")
            .unwrap()
            .members
            .contains(&"steve".to_string())
    );
    assert!(
        sb.team("blue")
            .unwrap()
            .members
            .contains(&"steve".to_string())
    );
}

#[test]
fn team_decorates_display_name() {
    let mut sb = Scoreboard::new();
    let mut team = Team::new("admins");
    team.prefix = Text::literal("[A] ");
    team.suffix = Text::literal("!");
    team.color = Some(TeamColor::Gold);
    sb.add_team(team);
    sb.add_member("admins", "notch");

    let name = sb.display_name_of("notch");
    assert_eq!(name.to_plain_string(), "[A] notch!");

    // A teamless holder gets a plain name.
    assert_eq!(sb.display_name_of("nobody").to_plain_string(), "nobody");
}

#[test]
fn removing_team_clears_reverse_index() {
    let mut sb = Scoreboard::new();
    sb.add_team(Team::new("red"));
    sb.add_member("red", "steve");
    sb.remove_team("red");
    assert!(sb.team_of("steve").is_none());
}

#[test]
fn score_entry_carries_display_and_format() {
    let mut sb = Scoreboard::new();
    sb.add_objective(obj("kills"));
    sb.set_score_entry(
        "kills",
        "alice",
        ScoreEntry {
            value: 3,
            display_name: Some(Text::literal("Alice the Brave")),
            ..Default::default()
        },
    );
    let e = sb.score("kills", "alice").unwrap();
    assert_eq!(e.value, 3);
    assert_eq!(
        e.display_name.as_ref().unwrap().to_plain_string(),
        "Alice the Brave"
    );
}
