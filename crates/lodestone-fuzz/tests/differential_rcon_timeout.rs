//! Hermetic wall-clock control for the blocking live-oracle transport.
#![cfg(feature = "rcon-oracle")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use lodestone_fuzz::differential::rcon::RconOracle;
use lodestone_fuzz::differential::{Action, WorldOracle};
use lodestone_testsupport::rcon_frame;

fn read_request(stream: &mut TcpStream) -> i32 {
    let mut len = [0; 4];
    stream.read_exact(&mut len).expect("read request length");
    let mut body = vec![0; i32::from_le_bytes(len) as usize];
    stream.read_exact(&mut body).expect("read request body");
    i32::from_le_bytes(body[0..4].try_into().expect("four-byte request id"))
}

#[test]
fn hostname_endpoint_is_rejected_before_connection_work() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind hostname rejection control");
    listener
        .set_nonblocking(true)
        .expect("make hostname rejection listener observable");
    let port = listener.local_addr().expect("read control address").port();
    let endpoint = format!("localhost:{port}");

    let error = RconOracle::connect_with_io_timeout(
        &endpoint,
        "unused",
        (0, 0, 0),
        Duration::from_millis(40),
    )
    .expect_err("hostnames must be rejected rather than resolved inside the connect deadline");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "endpoint validation must happen before issuing a connection"
    );
}

#[test]
fn a_server_that_accepts_but_never_answers_hits_the_configured_read_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stalled RCON control");
    let addr = listener.local_addr().expect("read stalled RCON address");
    let server = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().expect("accept RCON control connection");
        std::thread::sleep(Duration::from_millis(300));
    });

    let started = Instant::now();
    let error = RconOracle::connect_with_io_timeout(
        addr.to_string(),
        "unused",
        (0, 0, 0),
        Duration::from_millis(40),
    )
    .expect_err("a silent RCON peer must not block indefinitely");
    let elapsed = started.elapsed();

    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        elapsed < Duration::from_millis(250),
        "the 40 ms transport bound took {elapsed:?}"
    );
    server.join().expect("join stalled RCON control");
}

#[test]
fn a_drip_fed_frame_cannot_refresh_the_full_frame_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind drip-fed RCON control");
    let addr = listener.local_addr().expect("read drip-fed RCON address");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept RCON control connection");
        let auth_id = read_request(&mut stream);
        stream
            .write_all(&rcon_frame(auth_id, 2, ""))
            .expect("answer authentication promptly");

        let gametime_id = read_request(&mut stream);
        stream
            .write_all(&rcon_frame(gametime_id, 0, "The time is 1"))
            .expect("answer the initial game-time query promptly");

        let command_id = read_request(&mut stream);
        let response = rcon_frame(command_id, 0, "drip-fed");
        for byte in response {
            if stream.write_all(&[byte]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    });

    let mut oracle = RconOracle::connect_with_io_timeout(
        addr.to_string(),
        "unused",
        (0, 0, 0),
        Duration::from_millis(60),
    )
    .expect("the prompt authentication response should connect");
    let started = Instant::now();
    let error = oracle
        .apply(&Action::RunCommand("control".to_owned()))
        .expect_err("a drip-fed command response must hit the full-frame deadline");
    let elapsed = started.elapsed();
    drop(oracle);

    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        elapsed < Duration::from_millis(180),
        "the 60 ms full-frame bound took {elapsed:?}"
    );
    server.join().expect("join drip-fed RCON control");
}
