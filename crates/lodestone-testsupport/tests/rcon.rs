use std::time::Duration;

#[test]
fn rcon_frame_contains_length_header_and_complete_payload_in_one_buffer() {
    let frame = lodestone_testsupport::rcon_frame(7, 2, "say hi");
    assert_eq!(&frame[..4], &(16_i32).to_le_bytes());
    assert_eq!(&frame[4..8], &7_i32.to_le_bytes());
    assert_eq!(&frame[8..12], &2_i32.to_le_bytes());
    assert_eq!(&frame[12..], b"say hi\0\0");
}

#[tokio::test]
async fn poll_until_waits_for_condition_instead_of_asserting_immediately() {
    let mut attempts = 0_u8;
    let value = lodestone_testsupport::poll_until(
        Duration::from_millis(200),
        Duration::from_millis(1),
        || {
            attempts += 1;
            let ready = attempts >= 3;
            async move { ready.then_some(attempts) }
        },
    )
    .await
    .expect("condition eventually met");

    assert_eq!(value, 3);
    assert_eq!(attempts, 3);
}
