//! Fixed-seed model check for the 26.2 serverbound player-input packet.
//!
//! The shell reports continuous movement through `ClientAction::SetPlayerInput`.
//! `V770Adapter::encode_action` lowers that canonical input to the single-byte
//! `player_input` payload a server consumes. The expected byte below is an
//! independently owned bit layout, so this checks the action-to-wire boundary
//! rather than round-tripping the adapter through its own decoder.
//!
//! A deliberately wrong sprint bit is required to fail. That control makes a
//! green generated run evidence that the assertion reads the packed payload,
//! not merely that the adapter accepted every action.

#![cfg(feature = "v26-2")]

use lodestone_fuzz::catch;
use lodestone_model::{ClientAction, ConnectionState, PlayerInput, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestError, TestRunner};

const CASES: u32 = 256;
const SEED: u64 = 0x50_4c_41_59_45_52_49_4e;
const PLAYER_INPUT_PACKET_ID: i32 = 43;

fn input_strategy() -> impl Strategy<Value = PlayerInput> {
    (
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(forward, backward, left, right, jump, shift, sprint)| PlayerInput {
                forward,
                backward,
                left,
                right,
                jump,
                shift,
                sprint,
            },
        )
}

fn runner(cases: u32) -> TestRunner {
    TestRunner::new(Config {
        cases,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(SEED),
        failure_persistence: None,
        ..Config::default()
    })
}

/// Independent protocol-byte model: bits zero through six are the seven
/// canonical movement booleans in declaration order.
fn expected_flags(input: PlayerInput) -> u8 {
    u8::from(input.forward)
        | (u8::from(input.backward) << 1)
        | (u8::from(input.left) << 2)
        | (u8::from(input.right) << 3)
        | (u8::from(input.jump) << 4)
        | (u8::from(input.shift) << 5)
        | (u8::from(input.sprint) << 6)
}

fn wrong_sprint_bit_flags(input: PlayerInput) -> u8 {
    u8::from(input.forward)
        | (u8::from(input.backward) << 1)
        | (u8::from(input.left) << 2)
        | (u8::from(input.right) << 3)
        | (u8::from(input.jump) << 4)
        | (u8::from(input.shift) << 5)
        | (u8::from(input.sprint) << 7)
}

fn production_packet(input: PlayerInput) -> (i32, Vec<u8>) {
    catch(|| {
        V770Adapter::default().encode_action(
            ConnectionState::Play,
            &ClientAction::SetPlayerInput(input),
        )
    })
    .expect("player-input encoding must not panic")
    .expect("player-input encoding must succeed")
    .expect("player-input is valid in play state")
}

#[test]
fn generated_player_inputs_match_the_independent_packet_byte_model() {
    runner(CASES)
        .run(&input_strategy(), |input| {
            let (packet_id, body) = production_packet(input);
            prop_assert_eq!(packet_id, PLAYER_INPUT_PACKET_ID);
            prop_assert_eq!(body, vec![expected_flags(input)], "input: {:?}", input);
            Ok(())
        })
        .expect("the adapter player-input payload must match the independent bit model");
}

#[test]
fn wrong_sprint_bit_detector_is_rejected() {
    let input = PlayerInput {
        sprint: true,
        ..PlayerInput::EMPTY
    };
    let failure = runner(1)
        .run(&Just(input), |input| {
            let (_, body) = production_packet(input);
            prop_assert_eq!(body, vec![wrong_sprint_bit_flags(input)]);
            Ok(())
        })
        .expect_err("placing sprint in bit seven must disagree with the production packet");

    match failure {
        TestError::Fail(_, minimal) => {
            assert_eq!(minimal, input);
            assert_eq!(expected_flags(minimal), 0b0100_0000);
            assert_eq!(wrong_sprint_bit_flags(minimal), 0b1000_0000);
        }
        TestError::Abort(reason) => panic!("the detector control must fail, not abort: {reason}"),
    }
}
