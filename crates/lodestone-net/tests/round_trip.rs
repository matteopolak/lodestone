//! Hermetic integration test driving a full packet round trip over the
//! in-memory transport, including a mid-stream compression switch. This mirrors
//! what a real login sequence does: uncompressed handshake/login packets, then
//! `login_compression`, then compressed play packets spanning the threshold.

use lodestone_net::{Connection, memory_pair};

/// A packet as `(id, fields)` for concise fixtures.
type Packet = (i32, Vec<u8>);

fn fixtures() -> (Vec<Packet>, Vec<Packet>) {
    // Pre-compression packets (handshake/login style: small).
    let pre: Vec<Packet> = vec![
        (0x00, vec![0xFB, 0x05, b'h', b'o', b's', b't']),
        (0x00, b"player-name".to_vec()),
        (0x02, vec![1, 2, 3, 4, 5, 6, 7, 8]),
    ];

    // Post-compression packets spanning the threshold boundary (T = 32).
    let post: Vec<Packet> = vec![
        (0x03, vec![9; 31]),                             // T - 1 -> sent raw
        (0x24, vec![7; 32]),                             // T     -> compressed
        (0x25, vec![3; 33]),                             // T + 1 -> compressed
        (0x26, (0..1000u32).map(|i| i as u8).collect()), // large -> compressed
        (0x27, vec![]),                                  // empty body -> raw
    ];

    (pre, post)
}

#[tokio::test]
async fn full_login_style_round_trip_across_compression_boundary() {
    const THRESHOLD: i32 = 32;
    let (client_io, server_io) = memory_pair();
    let mut client = Connection::new(client_io);
    let mut server = Connection::new(server_io);

    let (pre, post) = fixtures();

    // Phase 1: uncompressed.
    for (id, fields) in &pre {
        client.write_packet(*id, fields).await.unwrap();
    }
    for expected in &pre {
        let got = server.read_packet().await.unwrap().unwrap();
        assert_eq!(&got, expected, "uncompressed packet mismatch");
    }

    // Phase 2: enable compression on both ends mid-stream.
    client.set_compression(THRESHOLD);
    server.set_compression(THRESHOLD);
    assert_eq!(client.compression_threshold(), Some(THRESHOLD as usize));

    for (id, fields) in &post {
        client.write_packet(*id, fields).await.unwrap();
    }
    for expected in &post {
        let got = server.read_packet().await.unwrap().unwrap();
        assert_eq!(&got, expected, "compressed packet mismatch");
    }

    // Clean shutdown yields EOF.
    drop(client);
    assert!(server.read_packet().await.unwrap().is_none());
}

#[tokio::test]
async fn unknown_packets_can_be_skipped_via_raw_body() {
    let (client_io, server_io) = memory_pair();
    let mut client = Connection::new(client_io);
    let mut server = Connection::new(server_io);

    client
        .write_packet(0x7F, &[0xAA, 0xBB, 0xCC])
        .await
        .unwrap();

    // The server does not understand 0x7F; it reads the raw body and can
    // discard it wholesale without parsing.
    let raw = server.read_packet_raw().await.unwrap().unwrap();
    assert_eq!(raw, vec![0x7F, 0xAA, 0xBB, 0xCC]);
}

/// Guards the protocol-blind boundary (see the `connection` module docs): the
/// net layer must split off only the packet-id VarInt and treat the remaining
/// field bytes as opaque. This is what lets one connection/relay serve every
/// version and every server.
///
/// The inputs are chosen to be *discriminating*: every body here is structurally
/// invalid under a real packet schema — it begins with a VarInt claiming a
/// ~4 GiB array/string length with no data behind it, which any field-level
/// decoder would reject or try to allocate for. And the ids span values that are
/// meaningful play-packet ids in real versions (`0x00`, `0x1A`, `0x3E`) as well
/// as impossible ones (`i32::MAX`, `-1`). If anyone ever adds a
/// `match packet_id { .. }` or field validation into the codec/connection, at
/// least one of these pairs would error or hang instead of round-tripping — so
/// this test fails the moment the boundary erodes.
#[tokio::test]
async fn codec_is_protocol_blind() {
    // A hostile-looking field payload: VarInt 0xFFFFFFFF (decodes to -1 / a
    // ~4 GiB unsigned length) followed by no bytes. Valid as opaque data;
    // invalid as the front of any real length-prefixed field.
    let hostile_fields = [0xFF, 0xFF, 0xFF, 0xFF, 0x0F];
    let ids = [0x00, 0x01, 0x1A, 0x3E, 0x7F, i32::MAX, -1];

    for id in ids {
        let (client_io, server_io) = memory_pair();
        let mut client = Connection::new(client_io);
        let mut server = Connection::new(server_io);

        client.write_packet(id, &hostile_fields).await.unwrap();

        // Interpreted form: exact id and byte-identical, unvalidated fields.
        let (got_id, got_fields) = server.read_packet().await.unwrap().unwrap();
        assert_eq!(got_id, id, "packet id must pass through verbatim");
        assert_eq!(
            got_fields, hostile_fields,
            "field bytes must pass through opaque and unvalidated"
        );
    }

    // Raw form: the codec hands back exactly [VarInt id][fields] for an id and a
    // body it has no schema for, proving no field-level parsing occurred.
    let (client_io, server_io) = memory_pair();
    let mut client = Connection::new(client_io);
    let mut server = Connection::new(server_io);
    client.write_packet(0x2A, &hostile_fields).await.unwrap();
    let raw = server.read_packet_raw().await.unwrap().unwrap();
    assert_eq!(raw, [&[0x2A][..], &hostile_fields[..]].concat());
}
