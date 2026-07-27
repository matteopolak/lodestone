//! Hermetic tests for the version-free chat model: decoration, filtering,
//! trust, and the rolling feed.

use lodestone_game::chat::{
    ChatDecoration, ChatEntry, ChatFeed, ChatParameter, DisguisedChatMessage, FilterMask,
    MessageTrust, PlayerChatMessage, SystemMessage,
};
use lodestone_model::{Text, TextStyle};
use uuid::Uuid;

fn msg(sender_name: &str, body: &str) -> PlayerChatMessage {
    PlayerChatMessage {
        sender: Uuid::from_u128(1),
        index: 0,
        signed_content: body.into(),
        unsigned_content: None,
        timestamp_ms: 0,
        salt: 0,
        signature: Some(vec![1, 2, 3]),
        filter_mask: FilterMask::PassThrough,
        sender_name: Text::literal(sender_name),
        target_name: None,
    }
}

#[test]
fn vanilla_chat_decoration_renders_angle_brackets() {
    let m = msg("Steve", "hello world");
    let display = m.display(&ChatDecoration::vanilla_chat()).unwrap();
    // chat.type.text == "<%s> %s"
    assert_eq!(display.to_plain_string(), "<Steve> hello world");
}

#[test]
fn unsigned_content_is_shown_verbatim() {
    let mut m = msg("Steve", "raw");
    m.unsigned_content = Some(Text::literal("server-rendered"));
    let display = m.display(&ChatDecoration::vanilla_chat()).unwrap();
    assert_eq!(display.to_plain_string(), "<Steve> server-rendered");
}

#[test]
fn decoration_carries_style() {
    use lodestone_model::TextColor;
    let style = TextStyle {
        color: Some(TextColor::Gray),
        italic: Some(true),
        ..TextStyle::default()
    };
    let deco = ChatDecoration::new(
        "chat.type.text",
        vec![ChatParameter::Sender, ChatParameter::Content],
        style,
    );
    let display = msg("A", "b").display(&deco).unwrap();
    assert_eq!(display.style.italic, Some(true));
    assert_eq!(display.style.color, Some(TextColor::Gray));
}

#[test]
fn target_parameter_resolves_for_whispers() {
    let mut m = msg("Steve", "psst");
    m.target_name = Some(Text::literal("Alex"));
    // Use a key present in the table so substitution is observable; [Target, Content].
    let deco = ChatDecoration::new(
        "chat.type.text",
        vec![ChatParameter::Target, ChatParameter::Content],
        TextStyle::default(),
    );
    let display = m.display(&deco).unwrap();
    // Target (Alex) substitutes into the first slot, body into the second.
    assert_eq!(display.to_plain_string(), "<Alex> psst");
}

#[test]
fn missing_target_resolves_empty_not_panic() {
    let deco = ChatDecoration::new(
        "chat.type.text",
        vec![ChatParameter::Target, ChatParameter::Content],
        TextStyle::default(),
    );
    let display = msg("Steve", "hi").display(&deco).unwrap();
    // Target empty -> "<> hi"
    assert_eq!(display.to_plain_string(), "<> hi");
}

#[test]
fn partial_filter_mask_replaces_with_hash() {
    let mut m = msg("Steve", "badword");
    // filter chars 0..3
    m.filter_mask = FilterMask::Partial(vec![true, true, true, false, false, false, false]);
    let display = m.display(&ChatDecoration::vanilla_chat()).unwrap();
    assert_eq!(display.to_plain_string(), "<Steve> ###word");
}

#[test]
fn fully_filtered_message_is_hidden() {
    let mut m = msg("Steve", "secret");
    m.filter_mask = FilterMask::FullyFiltered;
    assert!(m.display(&ChatDecoration::vanilla_chat()).is_none());
}

#[test]
fn filter_mask_apply_semantics() {
    assert_eq!(
        FilterMask::PassThrough.apply("abc"),
        Some("abc".to_string())
    );
    assert_eq!(FilterMask::FullyFiltered.apply("abc"), None);
    assert_eq!(
        FilterMask::Partial(vec![false, true, false]).apply("abc"),
        Some("a#c".to_string())
    );
    // mask shorter than text: unmasked positions pass through.
    assert_eq!(
        FilterMask::Partial(vec![true]).apply("abc"),
        Some("#bc".to_string())
    );
}

#[test]
fn message_trust_is_not_secure() {
    assert!(!MessageTrust::Secure.is_not_secure());
    assert!(MessageTrust::NotSecure.is_not_secure());
    assert!(MessageTrust::Modified.is_not_secure());
}

#[test]
fn disguised_message_decorates_like_player() {
    let d = DisguisedChatMessage {
        content: Text::literal("announce"),
        sender_name: Text::literal("Server"),
        target_name: None,
    };
    let display = d.display(&ChatDecoration::vanilla_chat());
    assert_eq!(display.to_plain_string(), "<Server> announce");
}

#[test]
fn system_message_overlay_flag() {
    let s = SystemMessage {
        content: Text::literal("saved"),
        overlay: true,
    };
    assert!(s.overlay);
    assert_eq!(s.content.to_plain_string(), "saved");
}

#[test]
fn feed_keeps_order_and_trust() {
    let mut feed = ChatFeed::new();
    feed.push_player(Text::literal("first"), MessageTrust::Secure);
    feed.push_system(Text::literal("sys"));
    feed.push_player(Text::literal("third"), MessageTrust::NotSecure);
    assert_eq!(feed.len(), 3);

    let entries: Vec<&ChatEntry> = feed.iter().collect();
    match entries[0] {
        ChatEntry::Player { display, trust } => {
            assert_eq!(display.to_plain_string(), "first");
            assert_eq!(*trust, MessageTrust::Secure);
        }
        _ => panic!("expected player"),
    }
    assert!(matches!(entries[1], ChatEntry::System { .. }));
    match feed.latest().unwrap() {
        ChatEntry::Player { trust, .. } => assert_eq!(*trust, MessageTrust::NotSecure),
        _ => panic!("expected player"),
    }
}

#[test]
fn feed_evicts_oldest_at_capacity() {
    let mut feed = ChatFeed::with_capacity(2);
    feed.push_system(Text::literal("a"));
    feed.push_system(Text::literal("b"));
    feed.push_system(Text::literal("c"));
    assert_eq!(feed.len(), 2);
    let texts: Vec<String> = feed
        .iter()
        .map(|e| match e {
            ChatEntry::System { content } => content.to_plain_string(),
            ChatEntry::Player { display, .. } => display.to_plain_string(),
        })
        .collect();
    assert_eq!(texts, vec!["b".to_string(), "c".to_string()]);
}
