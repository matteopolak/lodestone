#![cfg(feature = "differential-campaign")]

use lodestone_fuzz::campaign::{
    CampaignConfig, CampaignStatus, Checkpoint, MAX_JSON_BYTES, Scenario, advance_with,
};
use lodestone_fuzz::differential::{
    DifferentialOutcome, Divergence, OracleFailure, OracleFailureKind, Side,
};

fn config() -> CampaignConfig {
    CampaignConfig {
        scenario: Scenario::Fluid,
        endpoint: "127.0.0.1:25571".to_owned(),
        seed: Scenario::Fluid.default_seed(),
        cases: 11,
        shrink_attempts: 8,
        timing_attempts: 3,
    }
}

fn timeout() -> DifferentialOutcome {
    DifferentialOutcome::OracleFailed(OracleFailure {
        tick: 0,
        side: Side::Right,
        kind: OracleFailureKind::Timeout,
        message: "injected missed tick boundary".to_owned(),
    })
}

fn divergence() -> DifferentialOutcome {
    DifferentialOutcome::Diverged(Divergence {
        tick: 0,
        pos: (0, 0, 0),
        left: Some("minecraft:air".to_owned()),
        right: Some("minecraft:water".to_owned()),
    })
}

#[test]
fn resumed_stream_and_accounting_match_one_uninterrupted_campaign() {
    let mut full = Checkpoint::new(config()).expect("valid campaign");
    let mut expected = Vec::new();
    advance_with(&mut full, 11, |script, _, _| {
        expected.push(script.clone());
        DifferentialOutcome::Agreed
    }, |_| Ok(())).expect("complete campaign");

    let mut split = Checkpoint::new(config()).expect("valid split campaign");
    let mut observed = Vec::new();
    let mut saved = String::new();
    advance_with(&mut split, 4, |script, _, _| {
        observed.push(script.clone());
        DifferentialOutcome::Agreed
    }, |state| {
        saved = state.to_json_pretty()?;
        Ok(())
    }).expect("first slice");
    assert_eq!(split.next_case_index, 4);
    assert_eq!(split.status, CampaignStatus::Ready);
    let mut resumed = Checkpoint::from_json(&saved, &config()).expect("resume saved boundary");
    advance_with(&mut resumed, 7, |script, _, _| {
        observed.push(script.clone());
        DifferentialOutcome::Agreed
    }, |_| Ok(())).expect("second slice");
    assert_eq!(observed, expected);
    assert_eq!(resumed, full);
    assert_eq!(full.accepted_cases, 11);
    assert_eq!(full.search.accepted_evaluations, 11);
    assert_eq!(full.search.accepted_ticks, expected.iter().map(|script| script.last_tick() + 7).sum::<u64>());
    assert_eq!(full.search.accepted_actions, expected.iter().map(|script| script.steps.len() as u64).sum::<u64>());
    assert_eq!(full.status, CampaignStatus::Complete);
    assert!(full.finding.is_none());
}

#[test]
fn retries_are_separate_from_accepted_case_tick_and_action_totals() {
    let mut checkpoint = Checkpoint::new(config()).expect("valid campaign");
    let mut calls = 0;
    let mut accepted = None;
    advance_with(&mut checkpoint, 1, |script, _, _| {
        calls += 1;
        if calls < 3 {
            timeout()
        } else {
            accepted = Some(script.clone());
            DifferentialOutcome::Agreed
        }
    }, |_| Ok(())).expect("bounded retry");
    let script = accepted.expect("third attempt accepted");
    assert_eq!(calls, 3);
    assert_eq!(checkpoint.accepted_cases, 1);
    assert_eq!(checkpoint.generated_evaluations, 1);
    assert_eq!(checkpoint.search.oracle_attempts, 3);
    assert_eq!(checkpoint.search.retry_attempts, 2);
    assert_eq!(checkpoint.search.oracle_failures, 2);
    assert_eq!(checkpoint.search.timing_failures, 2);
    assert_eq!(checkpoint.search.accepted_evaluations, 1);
    assert_eq!(checkpoint.search.accepted_ticks, script.last_tick() + 7);
    assert_eq!(checkpoint.search.accepted_actions, script.steps.len() as u64);
    Checkpoint::from_json(&checkpoint.to_json_pretty().expect("encode"), &config()).expect("valid accounting");
}

#[test]
fn timing_failure_never_advances_the_case_and_resume_retries_the_same_script() {
    let mut checkpoint = Checkpoint::new(config()).expect("valid campaign");
    let mut attempted = Vec::new();
    advance_with(&mut checkpoint, 11, |script, _, _| {
        attempted.push(script.clone());
        timeout()
    }, |_| Ok(())).expect("failure is a recorded outcome");
    assert_eq!(attempted.len(), 3);
    assert!(attempted.iter().all(|script| script == &attempted[0]));
    assert_eq!(checkpoint.next_case_index, 0);
    assert_eq!(checkpoint.accepted_cases, 0);
    assert_eq!(checkpoint.search.accepted_ticks, 0);
    assert_eq!(checkpoint.search.accepted_actions, 0);
    assert_eq!(checkpoint.status, CampaignStatus::OracleFailed);
    let json = checkpoint.to_json_pretty().expect("encode failure boundary");
    let mut resumed = Checkpoint::from_json(&json, &config()).expect("resume failure");
    let mut first = None;
    advance_with(&mut resumed, 1, |script, _, _| {
        first = Some(script.clone());
        DifferentialOutcome::Agreed
    }, |_| Ok(())).expect("retry failed case");
    assert_eq!(first.as_ref(), Some(&attempted[0]));
    assert_eq!(resumed.next_case_index, 1);
    assert_eq!(resumed.search.oracle_attempts, 4);
}

#[test]
fn non_timeout_failure_is_not_retried() {
    let mut checkpoint = Checkpoint::new(config()).expect("valid campaign");
    advance_with(&mut checkpoint, 11, |_, _, _| {
        DifferentialOutcome::OracleFailed(OracleFailure {
            tick: 0, side: Side::Right, kind: OracleFailureKind::Failure,
            message: "injected invalid baseline".to_owned(),
        })
    }, |_| Ok(())).expect("record instrument failure");
    assert_eq!(checkpoint.search.oracle_attempts, 1);
    assert_eq!(checkpoint.search.retry_attempts, 0);
    assert_eq!(checkpoint.search.timing_failures, 0);
    assert_eq!(checkpoint.status, CampaignStatus::OracleFailed);
}

#[test]
fn a_generated_divergence_is_shrunk_saved_and_confirmed_by_explicit_replay() {
    let mut checkpoint = Checkpoint::new(config()).expect("valid campaign");
    let mut snapshots = Vec::new();
    advance_with(&mut checkpoint, 11, |_, _, _| divergence(), |state| {
        snapshots.push(state.to_json_pretty()?);
        Ok(())
    }).expect("find and replay detector control");
    assert_eq!(checkpoint.status, CampaignStatus::Divergence);
    assert_eq!(checkpoint.accepted_cases, 1);
    assert_eq!(checkpoint.generated_evaluations, 1);
    assert!(checkpoint.shrink_evaluations <= 8);
    assert_eq!(checkpoint.search.oracle_attempts, 1 + checkpoint.shrink_evaluations);
    assert_eq!(checkpoint.replay.oracle_attempts, 1);
    assert_eq!(checkpoint.replay.accepted_ticks, 1);
    assert_eq!(checkpoint.finding.as_ref().expect("replay artifact").expected_divergence(),
        match divergence() {
            DifferentialOutcome::Diverged(value) => value,
            _ => unreachable!(),
        });
    assert_eq!(snapshots.len(), 2);
    assert_eq!(Checkpoint::from_json(&snapshots[0], &config()).expect("pre-replay checkpoint").status,
        CampaignStatus::ReplayUnconfirmed);
    Checkpoint::from_json(&snapshots[1], &config()).expect("confirmed finding checkpoint");
}

#[test]
fn unconfirmed_replay_can_resume_without_regenerating_or_replacing_the_finding() {
    let mut campaign_config = config();
    campaign_config.shrink_attempts = 0;
    let mut checkpoint = Checkpoint::new(campaign_config.clone()).expect("valid campaign");
    let mut calls = 0;
    advance_with(&mut checkpoint, 11, |_, _, _| {
        calls += 1;
        if calls == 1 { divergence() } else { DifferentialOutcome::Agreed }
    }, |_| Ok(())).expect("record unconfirmed replay");
    assert_eq!(checkpoint.status, CampaignStatus::ReplayUnconfirmed);
    assert_eq!(checkpoint.next_case_index, 0);
    let finding = checkpoint.finding.clone();
    let json = checkpoint.to_json_pretty().expect("encode");
    let mut resumed = Checkpoint::from_json(&json, &campaign_config).expect("resume unconfirmed finding");
    advance_with(&mut resumed, 11, |_, _, _| divergence(), |_| Ok(())).expect("confirm replay");
    assert_eq!(resumed.status, CampaignStatus::Divergence);
    assert_eq!(resumed.finding, finding);
    assert_eq!(resumed.generated_evaluations, 1);
    assert_eq!(resumed.replay.oracle_attempts, 2);
    assert_eq!(resumed.accepted_cases, 1);
}

#[test]
fn changed_or_inconsistent_resume_inputs_are_rejected_with_a_valid_control() {
    let checkpoint = Checkpoint::new(config()).expect("valid campaign");
    let json = checkpoint.to_json_pretty().expect("encode");
    Checkpoint::from_json(&json, &config()).expect("unchanged control is accepted");
    let mut changed = config();
    changed.seed += 1;
    assert!(Checkpoint::from_json(&json, &changed).is_err());
    let mut value: serde_json::Value = serde_json::from_str(&json).expect("JSON checkpoint");
    value["next_case_index"] = serde_json::json!(2);
    assert!(Checkpoint::from_json(&value.to_string(), &config()).is_err());
    value["next_case_index"] = serde_json::json!(0);
    value["generation_version"] = serde_json::json!(999);
    assert!(Checkpoint::from_json(&value.to_string(), &config()).is_err());
    assert!(Checkpoint::from_json(&" ".repeat(MAX_JSON_BYTES as usize + 1), &config()).is_err());
}

#[test]
fn configuration_and_zero_invocation_budget_fail_before_evaluation() {
    for endpoint in ["example.com:25571", "192.0.2.1:25571", "127.0.0.1:0"] {
        let mut invalid = config();
        invalid.endpoint = endpoint.to_owned();
        assert!(Checkpoint::new(invalid).is_err());
    }
    let mut checkpoint = Checkpoint::new(config()).expect("loopback control is accepted");
    let mut evaluations = 0;
    assert!(advance_with(&mut checkpoint, 0, |_, _, _| {
        evaluations += 1;
        DifferentialOutcome::Agreed
    }, |_| Ok(())).is_err());
    assert_eq!(evaluations, 0);
    advance_with(&mut checkpoint, 1, |_, _, _| {
        evaluations += 1;
        DifferentialOutcome::Agreed
    }, |_| Ok(())).expect("positive budget executes control");
    assert_eq!(evaluations, 1);
}
