//! Scripted counter controls for the production RCON comparison boundary.
#![cfg(feature = "rcon-oracle")]

use std::convert::Infallible;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;
use std::time::Duration;

use lodestone_fuzz::differential::rcon::RconOracle;
use lodestone_fuzz::differential::{
    Action, DifferentialOutcome, OracleFailureKind, Script, ScriptStep, Side,
    WorldOracle, run_differential,
};
use lodestone_testsupport::rcon_frame;

const CLOCK: &str = "time query gametime";
const EDIT: &str = "setblock 0 0 0 minecraft:water[level=0]";
const FIRST_WATER: &str = "execute if block 1 0 0 minecraft:water";
const FIRST_AIR: &str = "execute if block 1 0 0 minecraft:air";
const SECOND_WATER: &str = "execute if block 2 0 0 minecraft:water";
const SECOND_AIR: &str = "execute if block 2 0 0 minecraft:air";
type Reply = (&'static str, String);

fn clock(value: i64) -> Reply {
    (CLOCK, format!("The time is {value}"))
}

fn read_request(stream: &mut TcpStream) -> (i32, String) {
    let mut length = [0; 4];
    stream.read_exact(&mut length).expect("read request length");
    let length = i32::from_le_bytes(length);
    assert!((10..4096).contains(&length));
    let mut body = vec![0; length as usize];
    stream.read_exact(&mut body).expect("read request body");
    let id = i32::from_le_bytes(body[..4].try_into().expect("four-byte id"));
    let text = String::from_utf8(body[8..body.len() - 2].to_vec()).expect("UTF-8 command");
    (id, text)
}

fn scripted_oracle(replies: Vec<Reply>) -> (RconOracle, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind scripted peer");
    let address = listener.local_addr().expect("peer address");
    let peer = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept oracle");
        stream.set_read_timeout(Some(Duration::from_secs(2))).expect("bound peer reads");
        stream.set_write_timeout(Some(Duration::from_secs(2))).expect("bound peer writes");
        let (id, password) = read_request(&mut stream);
        assert_eq!(password, "unused");
        stream.write_all(&rcon_frame(id, 2, "")).expect("answer authentication");
        for (expected, response) in replies {
            let (id, command) = read_request(&mut stream);
            assert_eq!(command, expected);
            stream.write_all(&rcon_frame(id, 0, &response)).expect("answer command");
        }
    });
    let oracle = RconOracle::connect_with_io_timeout(
        address.to_string(), "unused", (0, 0, 0), Duration::from_secs(1),
    ).expect("connect scripted oracle");
    (oracle, peer)
}

#[derive(Default)]
struct DryOracle {
    actions: usize,
}

impl WorldOracle for DryOracle {
    type Error = Infallible;

    fn apply(&mut self, _action: &Action) -> Result<(), Self::Error> {
        self.actions += 1;
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn block_state(
        &mut self, _pos: (i32, i32, i32), candidates: &[String],
    ) -> Result<Option<String>, Self::Error> {
        Ok(candidates.iter().find(|state| state.as_str() == "minecraft:air").cloned())
    }
}

fn compare(replies: Vec<Reply>) -> (DifferentialOutcome, usize, u32) {
    let (mut right, peer) = scripted_oracle(replies);
    let mut left = DryOracle::default();
    let script = Script::new(vec![ScriptStep {
        tick: 0,
        action: Action::SetBlock {
            pos: (0, 0, 0), state: "minecraft:water[level=0]".to_owned(),
        },
    }]);
    let candidates = vec!["minecraft:water".to_owned(), "minecraft:air".to_owned()];
    let region = vec![((1, 0, 0), candidates.clone()), ((2, 0, 0), candidates)];
    let outcome = run_differential(&script, &region, &mut left, &mut right, 0);
    let missed = right.missed_deadlines();
    drop(right);
    peer.join().expect("every prescribed command must be consumed");
    (outcome, left.actions, missed)
}

fn assert_timing_failure(outcome: DifferentialOutcome) {
    let DifferentialOutcome::OracleFailed(failure) = outcome else {
        panic!("a crossed boundary must reject comparison: {outcome:?}");
    };
    assert_eq!(failure.tick, 0);
    assert_eq!(failure.side, Side::Right);
    assert_eq!(failure.kind, OracleFailureKind::Timeout);
    assert!(failure.message.contains("crossed a tick boundary"));
}

fn observation(first_wet: bool, final_counter: i64) -> Vec<Reply> {
    let mut replies = vec![
        clock(103), clock(103), (EDIT, "Changed the block".to_owned()), clock(103),
        clock(104), clock(104),
        (FIRST_WATER, if first_wet { "Test passed" } else { "Test failed" }.to_owned()),
    ];
    if !first_wet {
        replies.push((FIRST_AIR, "Test passed".to_owned()));
    }
    replies.extend([
        (SECOND_WATER, "Test failed".to_owned()),
        (SECOND_AIR, "Test passed".to_owned()),
        clock(final_counter),
    ]);
    replies
}

#[test]
fn exact_action_and_observation_boundaries_accept_agreement() {
    let (outcome, actions, missed) = compare(observation(false, 104));
    assert!(matches!(outcome, DifferentialOutcome::Agreed));
    assert_eq!(actions, 1);
    assert_eq!(missed, 0);
}

#[test]
fn exact_boundaries_keep_the_first_divergence_after_all_probes() {
    let (outcome, actions, missed) = compare(observation(true, 104));
    let DifferentialOutcome::Diverged(divergence) = outcome else {
        panic!("the scripted state difference must be detected: {outcome:?}");
    };
    assert_eq!(divergence.tick, 0);
    assert_eq!(divergence.pos, (1, 0, 0));
    assert_eq!(divergence.left.as_deref(), Some("minecraft:air"));
    assert_eq!(divergence.right.as_deref(), Some("minecraft:water"));
    assert_eq!(actions, 1);
    assert_eq!(missed, 0);
}

#[test]
fn a_single_tick_crossing_before_the_first_action_rejects_the_candidate() {
    let (outcome, actions, missed) = compare(vec![clock(103), clock(104)]);
    assert_timing_failure(outcome);
    assert_eq!(actions, 0);
    assert_eq!(missed, 1);
}

#[test]
fn a_single_tick_crossing_during_the_action_group_rejects_the_candidate() {
    let (outcome, actions, missed) = compare(vec![
        clock(103), clock(103), (EDIT, "Changed the block".to_owned()), clock(104),
    ]);
    assert_timing_failure(outcome);
    assert_eq!(actions, 1);
    assert_eq!(missed, 1);
}

#[test]
fn a_single_tick_crossing_during_the_first_divergent_probe_overrides_divergence() {
    let (outcome, _, missed) = compare(observation(true, 105));
    assert_timing_failure(outcome);
    assert_eq!(missed, 1);
}

#[test]
fn a_single_tick_crossing_during_matching_probes_cannot_report_agreement() {
    let (outcome, _, missed) = compare(observation(false, 105));
    assert_timing_failure(outcome);
    assert_eq!(missed, 1);
}

#[test]
fn comparison_advancement_rejects_overshoot_immediately() {
    let (outcome, _, missed) = compare(vec![
        clock(103), clock(103), (EDIT, "Changed the block".to_owned()), clock(103),
        clock(105),
    ]);
    assert_timing_failure(outcome);
    assert_eq!(missed, 1);
}

#[test]
fn setup_waits_and_a_reset_after_comparison_keep_non_strict_clock_behavior() {
    let (mut oracle, peer) = scripted_oracle(vec![
        clock(103), clock(107), clock(110), clock(113),
    ]);
    oracle.advance_tick().expect("setup may span several ticks");
    assert_eq!(oracle.missed_deadlines(), 3);
    oracle.begin_comparison().expect("enable strict comparison");
    oracle.reset_baseline().expect("cleanup resets comparison mode");
    oracle.advance_tick().expect("cleanup may span several ticks");
    assert_eq!(oracle.missed_deadlines(), 2);
    drop(oracle);
    peer.join().expect("consume setup and cleanup counter replies");
}
