use super::*;

    // ---- scoreboard: the two members with no dedicated schedule test ------

    fn red_team_params() -> Box<TeamParameters> {
        Box::new(TeamParameters {
            display_name: Text::literal("Red Team"),
            prefix: Text::literal("["),
            suffix: Text::literal("]"),
            name_tag_visibility: Visibility::HideForOtherTeams,
            collision_rule: CollisionRule::PushOwnTeam,
            color: Some(TeamColor::Red),
            friendly_fire: false,
            see_friendly_invisibles: true,
        })
    }

    /// `ScoreUpdate`/`ObjectiveUpdate`/`DisplayObjective` already have a
    /// real-schedule test above (`a_net_ingest_run_folds_a_scoreboard_objective
    /// _onto_the_component`); `ScoreReset` and `TeamUpdate` shared the same
    /// `apply_scoreboard` system but had no schedule-level test of their own.
    #[test]
    fn score_reset_and_team_update_reach_the_scoreboard_through_the_real_schedule() {
        let (mut app, entity) = session_app();
        fold(
            &mut app,
            ClientEvent::ObjectiveUpdate {
                name: "kills".into(),
                mode: ObjectiveMode::Add,
                display_name: Some(Text::literal("Kills")),
                render_type: None,
                number_format: None,
            },
        );
        fold(
            &mut app,
            ClientEvent::ScoreUpdate {
                holder: "Alice".into(),
                objective: "kills".into(),
                value: 7,
                display: None,
                number_format: None,
            },
        );
        assert_eq!(
            app.world()
                .get::<SessionScoreboard>(entity)
                .unwrap()
                .0
                .score("kills", "Alice")
                .map(|e| e.value),
            Some(7),
            "precondition: the score exists before it is reset"
        );

        fold(
            &mut app,
            ClientEvent::ScoreReset {
                holder: "Alice".into(),
                objective: Some("kills".into()),
            },
        );
        assert_eq!(
            app.world()
                .get::<SessionScoreboard>(entity)
                .unwrap()
                .0
                .score("kills", "Alice")
                .map(|e| e.value),
            None,
            "ScoreReset must reach the scoreboard through the real schedule"
        );

        fold(
            &mut app,
            ClientEvent::TeamUpdate {
                name: "red".into(),
                action: TeamAction::Create {
                    params: red_team_params(),
                    members: vec!["Bob".into()],
                },
            },
        );
        let board = &app.world().get::<SessionScoreboard>(entity).unwrap().0;
        assert_eq!(board.team_of("Bob").map(|t| t.name.as_str()), Some("red"));
    }
