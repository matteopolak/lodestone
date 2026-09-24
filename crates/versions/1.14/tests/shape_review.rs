const SHAPE_REVIEW: &str = include_str!("../SHAPE_REVIEW.toml");

#[test]
fn packet_shape_review_is_complete() {
    let mut current_packet = "<unknown packet>";
    for line in SHAPE_REVIEW.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("name = ") {
            current_packet = value.trim_matches('"');
        } else if trimmed == "reviewed = false" {
            panic!(
                "packet shape review is incomplete for {current_packet}; audit the codec against this protocol, then set reviewed = true in SHAPE_REVIEW.toml"
            );
        }
    }
}
