//! libFuzzer target: the packet receive framing state machine must never panic
//! or hang on an arbitrary, fragmented byte stream.
//!
//! Packet-specific targets begin with a decoded `(state, id, payload)`, which
//! leaves the earlier untrusted boundary untested: a peer controls the frame
//! length VarInt, compression header, zlib stream, and where the transport
//! splits all of them. This target feeds one bounded fragment at a time into
//! [`lodestone_net::Codec`], draining every complete packet after each feed.
//! That reaches the partial-prefix and partial-body states a whole-buffer
//! decoder cannot observe.
//!
//! Input layout:
//! - byte 0 selects compression: disabled, threshold 1, or threshold 256;
//! - byte 1 selects a repeatable fragment size from 1 through 32 bytes;
//! - remaining bytes are the hostile stream.
//!
//! A decoder error is a valid response to malformed input and ends this case.
//! A panic or an input that makes the state machine fail to make progress is
//! the finding. The committed seeds are captured packet payloads framed by an
//! independent Python VarInt/zlib writer, rather than `Codec::encode`; see
//! `fuzz/seeds/generate-seeds.py` and `docs/fuzzing.md`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestone_net::Codec;

fn set_mode(codec: &mut Codec, selector: u8) {
    match selector % 3 {
        0 => {}
        1 => codec.set_compression(1),
        2 => codec.set_compression(256),
        _ => unreachable!("modulo three has exactly three outcomes"),
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }

    let mut codec = Codec::new();
    set_mode(&mut codec, data[0]);
    let fragment_len = usize::from(data[1] % 32) + 1;
    let stream = &data[2..];

    for fragment in stream.chunks(fragment_len) {
        codec.feed(fragment);
        loop {
            match codec.next_packet() {
                Ok(Some(_body)) => {}
                Ok(None) => break,
                Err(_) => return,
            }
        }
    }
});
