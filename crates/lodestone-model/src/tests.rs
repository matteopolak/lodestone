use std::str::FromStr;

use uuid::Uuid;

use super::*;

#[test]
fn vec3_supports_basic_math() {
    let a = Vec3::new(3.0, 4.0, 12.0);
    let b = Vec3::new(1.0, -2.0, 0.5);

    assert_eq!(a + b, Vec3::new(4.0, 2.0, 12.5));
    assert_eq!(a - b, Vec3::new(2.0, 6.0, 11.5));
    assert_eq!(b.scale(2.0), Vec3::new(2.0, -4.0, 1.0));
    assert_eq!(a.length(), 13.0);
    assert_eq!(a.dot(b), 1.0);
    assert_eq!(Vec3::default().normalize(), Vec3::default());

    let normalized = a.normalize();
    assert!((normalized.length() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn vec3f_supports_basic_math() {
    let a = Vec3f::new(0.0, 3.0, 4.0);
    let b = Vec3f::new(2.0, -1.0, 0.5);

    assert_eq!(a + b, Vec3f::new(2.0, 2.0, 4.5));
    assert_eq!(a - b, Vec3f::new(-2.0, 4.0, 3.5));
    assert_eq!(b.scale(4.0), Vec3f::new(8.0, -4.0, 2.0));
    assert_eq!(a.length(), 5.0);
    assert_eq!(a.dot(b), -1.0);
    assert_eq!(Vec3f::default().normalize(), Vec3f::default());
}

#[test]
fn block_positions_floor_divide_into_chunks_and_sections() {
    let cases = [
        (
            BlockPos::new(0, 0, 0),
            ChunkPos::new(0, 0),
            SectionPos::new(0, 0, 0),
        ),
        (
            BlockPos::new(15, 15, 15),
            ChunkPos::new(0, 0),
            SectionPos::new(0, 0, 0),
        ),
        (
            BlockPos::new(16, 16, 16),
            ChunkPos::new(1, 1),
            SectionPos::new(1, 1, 1),
        ),
        (
            BlockPos::new(-1, -1, -1),
            ChunkPos::new(-1, -1),
            SectionPos::new(-1, -1, -1),
        ),
        (
            BlockPos::new(-16, -16, -16),
            ChunkPos::new(-1, -1),
            SectionPos::new(-1, -1, -1),
        ),
        (
            BlockPos::new(-17, -17, -17),
            ChunkPos::new(-2, -2),
            SectionPos::new(-2, -2, -2),
        ),
    ];

    for (block, chunk, section) in cases {
        assert_eq!(block.chunk_pos(), chunk);
        assert_eq!(block.section_pos(), section);
        assert_eq!(ChunkPos::from(block), chunk);
        assert_eq!(SectionPos::from(block), section);
        assert_eq!(section.chunk_pos(), chunk);
    }
}

#[test]
fn chunk_and_section_origins_are_block_positions() {
    assert_eq!(ChunkPos::new(-2, 3).block_min(), BlockPos::new(-32, 0, 48));
    assert_eq!(
        SectionPos::new(-2, -1, 3).block_min(),
        BlockPos::new(-32, -16, 48)
    );
}

#[test]
fn resource_keys_parse_default_namespace_and_display_canonically() {
    let implicit = ResourceKey::from_str("stone").expect("implicit namespace");
    let explicit = ResourceKey::from_str("minecraft:stone").expect("explicit namespace");
    let custom = ResourceKey::from_str("lodestone:blocks/polished_test-stone").expect("custom key");

    assert_eq!(implicit, explicit);
    assert_eq!(implicit.namespace(), "minecraft");
    assert_eq!(implicit.path(), "stone");
    assert_eq!(implicit.to_string(), "minecraft:stone");
    assert_eq!(custom.to_string(), "lodestone:blocks/polished_test-stone");
}

#[test]
fn resource_keys_reject_invalid_forms() {
    for invalid in [
        "",
        ":stone",
        "minecraft:",
        "minecraft:bad:path",
        "MineCraft:stone",
        "minecraft:Stone",
        "minecraft:bad path",
        "minecraft:bad#path",
    ] {
        assert!(
            ResourceKey::from_str(invalid).is_err(),
            "{invalid} should be invalid"
        );
    }
}

#[test]
fn text_flattens_nested_extra_children() {
    let mut root = Text::literal("Hello");
    let mut child = Text::literal(", ");
    child.extra.push(Text::literal("world"));
    root.extra.push(child);
    root.extra.push(Text::literal("!"));

    assert_eq!(root.to_plain_string(), "Hello, world!");
}

#[test]
fn chat_events_carry_acknowledgement_inputs_for_signed_messages() {
    let shown = ClientEvent::Chat {
        text: Text::literal("hello"),
        kind: ChatKind::Chat,
        sender: None,
        ack: Some(ChatAckInfo {
            signature: vec![1, 2, 3, 4],
            global_index: 12,
            was_shown: true,
            message_index: 0,
            timestamp_millis: 1_700_000_000_000,
            salt: 42,
            raw_content: "hello".to_string(),
            last_seen: Vec::new(),
            verified: false,
        }),
    };
    let filtered = ClientEvent::Chat {
        text: Text::literal("filtered"),
        kind: ChatKind::Chat,
        sender: None,
        ack: Some(ChatAckInfo {
            signature: vec![5, 6, 7, 8],
            global_index: 13,
            was_shown: false,
            message_index: 1,
            timestamp_millis: 1_700_000_001_000,
            salt: 43,
            raw_content: "filtered".to_string(),
            last_seen: Vec::new(),
            verified: false,
        }),
    };

    assert!(matches!(
        shown,
        ClientEvent::Chat {
            ack: Some(ChatAckInfo {
                signature,
                global_index: 12,
                was_shown: true,
                ..
            }),
            ..
        } if signature == vec![1, 2, 3, 4]
    ));
    assert!(matches!(
        filtered,
        ClientEvent::Chat {
            ack: Some(ChatAckInfo {
                signature,
                global_index: 13,
                was_shown: false,
                ..
            }),
            ..
        } if signature == vec![5, 6, 7, 8]
    ));
}

#[test]
fn legacy_text_parser_tracks_color_and_format_segments() {
    let text = Text::from_legacy("Plain §cRed §lBold§r Normal");

    assert_eq!(text.to_plain_string(), "Plain Red Bold Normal");
    assert_eq!(text.extra[1].content, TextContent::Literal("Red ".into()));
    assert_eq!(text.extra[1].style.color, Some(TextColor::Red));
    // A colour code clears formatting *explicitly*, not by leaving it
    // unspecified — `Style.applyLegacyFormat`'s `default:` arm assigns
    // `bold = false`. `None` here would let an enclosing component's bold
    // inherit through the reset, which vanilla does not do; see
    // `apply_legacy_code`.
    assert_eq!(text.extra[1].style.bold, Some(false));
    assert_eq!(text.extra[1].style.italic, Some(false));
    assert_eq!(text.extra[2].content, TextContent::Literal("Bold".into()));
    assert_eq!(text.extra[2].style.color, Some(TextColor::Red));
    assert_eq!(text.extra[2].style.bold, Some(true));
    assert_eq!(
        text.extra[3].content,
        TextContent::Literal(" Normal".into())
    );
    assert_eq!(text.extra[3].style, TextStyle::default());
}

#[test]
fn style_inheritance_uses_child_value_then_parent() {
    // Parent turns bold on and sets red; child leaves bold unspecified (should
    // inherit true) but explicitly turns italic on and colour blue.
    let parent = TextStyle {
        font: None,
        color: Some(TextColor::Red),
        bold: Some(true),
        ..TextStyle::default()
    };
    let child = TextStyle {
        font: None,
        color: Some(TextColor::Blue),
        italic: Some(true),
        ..TextStyle::default()
    };
    let resolved = child.inherit(&parent);
    assert_eq!(resolved.color, Some(TextColor::Blue)); // child overrides
    assert_eq!(resolved.bold, Some(true)); // inherited
    assert_eq!(resolved.italic, Some(true)); // child's own
    assert_eq!(resolved.underlined, None); // neither set
}

#[test]
fn explicit_false_is_not_inherited_over() {
    // The load-bearing case: a child that explicitly disables bold must stay
    // not-bold even though the parent is bold. Some(false) != None.
    let parent = TextStyle {
        font: None,
        bold: Some(true),
        ..TextStyle::default()
    };
    let child = TextStyle {
        font: None,
        bold: Some(false),
        ..TextStyle::default()
    };
    assert_eq!(child.inherit(&parent).bold, Some(false));
}

#[test]
fn to_spans_resolves_inheritance_down_the_tree() {
    // root(bold) -> child(color=red, bold unspecified) -> grandchild(bold=false)
    let text = Text {
        content: TextContent::Literal("A".into()),
        style: TextStyle {
            font: None,
            bold: Some(true),
            ..TextStyle::default()
        },
        extra: vec![Text {
            content: TextContent::Literal("B".into()),
            style: TextStyle {
                font: None,
                color: Some(TextColor::Red),
                ..TextStyle::default()
            },
            extra: vec![Text {
                content: TextContent::Literal("C".into()),
                style: TextStyle {
                    font: None,
                    bold: Some(false),
                    ..TextStyle::default()
                },
                ..Text::default()
            }],
            ..Text::default()
        }],
        ..Text::default()
    };
    let spans = text.to_spans();
    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0].text, "A");
    assert_eq!(spans[0].style.bold, Some(true));
    assert_eq!(spans[1].text, "B");
    assert_eq!(spans[1].style.bold, Some(true)); // inherited from A
    assert_eq!(spans[1].style.color, Some(TextColor::Red));
    assert_eq!(spans[2].text, "C");
    assert_eq!(spans[2].style.bold, Some(false)); // explicit override wins
    assert_eq!(spans[2].style.color, Some(TextColor::Red)); // inherited from B
}

/// `click_event`/`hover_event` decode onto [`Text::click`]/[`Text::hover`]
/// correctly (`from_json_parses_styles_extra_and_events` already proves that),
/// but [`Text::to_spans`] never read them — a hover tooltip or a link click
/// had nowhere to reach once a tree was flattened for rendering.
/// [`Text::to_interactive_spans`] is the fix: same tree, same
/// child-overrides-parent inheritance [`to_spans_resolves_inheritance_down_the_tree`]
/// already pins for style, extended to `click`/`hover`/`insertion`.
#[test]
fn to_interactive_spans_inherits_click_hover_and_insertion_down_the_tree() {
    let open_url = ClickEvent {
        action: ClickAction::OpenUrl,
        value: "https://example.invalid".into(),
    };
    let run_command = ClickEvent {
        action: ClickAction::RunCommand,
        value: "/help".into(),
    };
    let tooltip = HoverEvent {
        action: HoverAction::ShowText,
        value: Box::new(Text::literal("a tooltip")),
    };
    // root(click=open_url, hover=tooltip, insertion="root")
    //   -> child A(no events of its own -- must inherit all three)
    //   -> child B(click=run_command -- overrides click, still inherits hover/insertion)
    let text = Text {
        content: TextContent::Literal("root".into()),
        click: Some(open_url.clone()),
        hover: Some(tooltip.clone()),
        insertion: Some("root".into()),
        extra: vec![
            Text {
                content: TextContent::Literal("A".into()),
                ..Text::default()
            },
            Text {
                content: TextContent::Literal("B".into()),
                click: Some(run_command.clone()),
                ..Text::default()
            },
        ],
        ..Text::default()
    };
    let spans = text.to_interactive_spans();
    assert_eq!(spans.len(), 3);

    assert_eq!(spans[0].text, "root");
    assert_eq!(spans[0].click, Some(open_url.clone()));
    assert_eq!(spans[0].hover, Some(tooltip.clone()));
    assert_eq!(spans[0].insertion, Some("root".into()));

    assert_eq!(spans[1].text, "A");
    assert_eq!(spans[1].click, Some(open_url.clone()), "A must inherit root's click");
    assert_eq!(spans[1].hover, Some(tooltip.clone()), "A must inherit root's hover");
    assert_eq!(spans[1].insertion, Some("root".into()), "A must inherit root's insertion");

    assert_eq!(spans[2].text, "B");
    assert_eq!(spans[2].click, Some(run_command), "B's own click must win over root's");
    assert_eq!(spans[2].hover, Some(tooltip), "B must still inherit root's hover");
    assert_eq!(spans[2].insertion, Some("root".into()), "B must still inherit root's insertion");
}

/// A legacy `§`-coded run that also carries a click/hover: the code-split
/// pieces [`Text::to_interactive_spans`] produces must all carry the *outer*
/// span's click/hover, since the re-parsed inner text
/// (`Text::from_legacy(&span.text)`) has none of its own — the same
/// find-the-discriminating-input shape as `to_spans`'s own legacy-splitting
/// tests, extended to prove the events survive the split rather than only the
/// style.
#[test]
fn to_interactive_spans_carries_click_through_a_legacy_code_split() {
    let open_url = ClickEvent {
        action: ClickAction::OpenUrl,
        value: "https://example.invalid".into(),
    };
    let text = Text {
        content: TextContent::Literal("plain \u{a7}claink".into()),
        click: Some(open_url.clone()),
        ..Text::default()
    };
    let spans = text.to_interactive_spans();
    assert!(spans.len() >= 2, "the §c must split this into at least two runs");
    for span in &spans {
        assert_eq!(
            span.click,
            Some(open_url.clone()),
            "every legacy-split piece of {:?} must still carry the outer click",
            span.text
        );
    }
}

#[test]
fn translate_substitutes_sequential_and_indexed_args() {
    let joined = Text::translate(
        "multiplayer.player.joined",
        vec![Text::literal("Lodestone")],
    );
    assert_eq!(joined.to_plain_string(), "Lodestone joined the game");

    let chat = Text::translate(
        "chat.type.text",
        vec![Text::literal("Lodestone"), Text::literal("hi there")],
    );
    assert_eq!(chat.to_plain_string(), "<Lodestone> hi there");

    // Indexed %1$s / %2$s.
    let death = Text::translate(
        "death.attack.mob",
        vec![Text::literal("Lodestone"), Text::literal("Zombie")],
    );
    assert_eq!(death.to_plain_string(), "Lodestone was slain by Zombie");
}

#[test]
fn unknown_translate_key_falls_back_to_fallback_then_key() {
    let with_fallback = Text {
        content: TextContent::Translate {
            key: "some.unknown.key".into(),
            with: vec![Text::literal("X")],
            fallback: Some("fallback %s".into()),
        },
        ..Text::default()
    };
    assert_eq!(with_fallback.to_plain_string(), "fallback X");

    let no_fallback = Text::translate("some.unknown.key", vec![Text::literal("X")]);
    assert_eq!(no_fallback.to_plain_string(), "some.unknown.key");
}

#[test]
fn from_json_parses_styles_extra_and_events() {
    let json = r#"{
        "text": "Hello ",
        "bold": true,
        "color": "gold",
        "clickEvent": {"action": "open_url", "value": "https://example.com"},
        "extra": [
            {"text": "world", "italic": true, "color": "red"},
            "!"
        ]
    }"#;
    let text = Text::from_json(json);
    assert_eq!(text.to_plain_string(), "Hello world!");
    assert_eq!(text.style.color, Some(TextColor::Gold));
    assert_eq!(text.style.bold, Some(true));
    assert_eq!(
        text.click,
        Some(ClickEvent {
            action: ClickAction::OpenUrl,
            value: "https://example.com".into(),
        })
    );
    assert_eq!(text.extra[0].style.italic, Some(true));
    assert_eq!(text.extra[0].style.color, Some(TextColor::Red));
    assert_eq!(text.extra[1].content, TextContent::Literal("!".into()));
}

#[test]
fn from_json_array_makes_first_element_parent() {
    let text = Text::from_json(r#"["a", "b", "c"]"#);
    assert_eq!(text.to_plain_string(), "abc");
    assert_eq!(text.content, TextContent::Literal("a".into()));
    assert_eq!(text.extra.len(), 2);
}

#[test]
fn from_json_hex_color() {
    let text = Text::from_json(r##"{"text":"x","color":"#ff8800"}"##);
    assert_eq!(text.style.color, Some(TextColor::Rgb(0xff_8800)));
}

#[test]
fn from_json_malformed_falls_back_to_literal() {
    assert_eq!(Text::from_json("{not json").to_plain_string(), "{not json");
}

#[test]
fn from_nbt_mirrors_json_for_equivalent_component() {
    use lodestone_core::{Nbt, NbtTag};
    // { text: "Hello ", bold: 1b, color: "gold", extra: [ {text:"world", color:"red"} ] }
    let nbt = Nbt::Compound(vec![
        ("text".into(), Nbt::String("Hello ".into())),
        ("bold".into(), Nbt::Byte(1)),
        ("color".into(), Nbt::String("gold".into())),
        (
            "extra".into(),
            Nbt::List {
                element_type: NbtTag::Compound,
                elements: vec![Nbt::Compound(vec![
                    ("text".into(), Nbt::String("world".into())),
                    ("color".into(), Nbt::String("red".into())),
                ])],
            },
        ),
    ]);
    let from_nbt = Text::from_nbt(&nbt);
    let from_json = Text::from_json(
        r#"{"text":"Hello ","bold":true,"color":"gold","extra":[{"text":"world","color":"red"}]}"#,
    );
    // Both front-ends produce the same tree.
    assert_eq!(from_nbt, from_json);
    assert_eq!(from_nbt.to_plain_string(), "Hello world");
    assert_eq!(from_nbt.style.bold, Some(true));
    assert_eq!(from_nbt.style.color, Some(TextColor::Gold));
}

#[test]
fn from_nbt_translate_join_matches_json_translate_join() {
    use lodestone_core::{Nbt, NbtTag};
    // Modern servers send the join message as an NBT translate component.
    let nbt = Nbt::Compound(vec![
        (
            "translate".into(),
            Nbt::String("multiplayer.player.joined".into()),
        ),
        (
            "with".into(),
            Nbt::List {
                element_type: NbtTag::String,
                elements: vec![Nbt::String("Lodestone".into())],
            },
        ),
    ]);
    // A 1.8 server sends the same message as a JSON translate component.
    let json = r#"{"translate":"multiplayer.player.joined","with":["Lodestone"]}"#;

    // The cross-format oracle: semantically identical messages in the two wire
    // formats flatten to the same plain text.
    assert_eq!(
        Text::from_nbt(&nbt).to_plain_string(),
        Text::from_json(json).to_plain_string(),
    );
    assert_eq!(
        Text::from_nbt(&nbt).to_plain_string(),
        "Lodestone joined the game"
    );
}

#[test]
fn legacy_round_trips_through_to_legacy_string() {
    let original = "§cRed §lBold";
    let text = Text::from_legacy(original);
    // Rendering resolved spans back to legacy codes preserves the visible text
    // and colour/format state.
    let rendered = text.to_legacy_string();
    assert_eq!(Text::from_legacy(&rendered).to_plain_string(), "Red Bold");
    assert!(rendered.contains("§c"));
    assert!(rendered.contains("§l"));
}

#[test]
fn deeply_nested_json_does_not_panic() {
    let deep = format!("{}\"x\"{}", "[".repeat(200), "]".repeat(200));
    let _ = Text::from_json(&deep);
}

#[test]
fn adapter_directives_express_configuration_login_choreography() {
    #[derive(Debug)]
    struct ConfigurationLoginAdapter;

    impl VersionAdapter for ConfigurationLoginAdapter {
        fn protocol_version(&self) -> i32 {
            2
        }

        fn minecraft_versions(&self) -> &'static [&'static str] {
            &["configuration-test"]
        }

        fn supports(&self, protocol: i32) -> bool {
            protocol == self.protocol_version()
        }

        fn begin_login(
            &self,
            profile: &LoginProfile,
            server: &ServerAddress,
        ) -> Result<Vec<Directive>, AdapterError> {
            assert_eq!(profile.username, "Steve");
            assert_eq!(server.host, "example.org");
            assert_eq!(server.port, 25565);

            Ok(vec![
                Directive::Send {
                    packet_id: 0,
                    payload: vec![1],
                },
                Directive::Send {
                    packet_id: 1,
                    payload: profile.uuid.as_bytes().to_vec(),
                },
            ])
        }

        fn handle_packet(
            &self,
            _world: &mut dyn lodestone_world::WorldSink,
            state: ConnectionState,
            packet_id: i32,
            _payload: &[u8],
        ) -> Result<Vec<Directive>, AdapterError> {
            Ok(match (state, packet_id) {
                (ConnectionState::Login, 2) => vec![Directive::SetCompression(256)],
                (ConnectionState::Login, 3) => vec![
                    Directive::Send {
                        packet_id: 4,
                        payload: vec![],
                    },
                    Directive::SetState(ConnectionState::Configuration),
                ],
                (ConnectionState::Configuration, 5) => vec![Directive::Send {
                    packet_id: 6,
                    payload: vec![7],
                }],
                (ConnectionState::Configuration, 8) => vec![],
                (ConnectionState::Configuration, 9) => vec![Directive::Send {
                    packet_id: 10,
                    payload: vec![],
                }],
                (ConnectionState::Configuration, 11) => vec![
                    Directive::Send {
                        packet_id: 12,
                        payload: vec![],
                    },
                    Directive::SetState(ConnectionState::Play),
                ],
                _ => Vec::new(),
            })
        }

        fn encode_action(
            &self,
            state: ConnectionState,
            action: &ClientAction,
        ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
            assert_eq!(state, ConnectionState::Play);
            assert_eq!(action, &ClientAction::KeepAliveResponse { id: 42 });
            Ok(Some((8, vec![4, 5, 6])))
        }
    }

    let adapter = ConfigurationLoginAdapter;
    let profile = LoginProfile {
        username: "Steve".to_owned(),
        uuid: Uuid::from_u128(0x12345678123456781234567812345678),
    };
    let server = ServerAddress {
        host: "example.org".to_owned(),
        port: 25565,
    };

    assert_eq!(
        adapter.begin_login(&profile, &server).expect("begin login"),
        vec![
            Directive::Send {
                packet_id: 0,
                payload: vec![1],
            },
            Directive::Send {
                packet_id: 1,
                payload: profile.uuid.as_bytes().to_vec(),
            },
        ]
    );
    assert_eq!(
        adapter
            .handle_packet(
                &mut lodestone_world::World::new(),
                ConnectionState::Login,
                2,
                &[]
            )
            .expect("compression"),
        vec![Directive::SetCompression(256)]
    );
    assert_eq!(
        adapter
            .handle_packet(
                &mut lodestone_world::World::new(),
                ConnectionState::Login,
                3,
                &[]
            )
            .expect("login finished"),
        vec![
            Directive::Send {
                packet_id: 4,
                payload: vec![],
            },
            Directive::SetState(ConnectionState::Configuration),
        ]
    );
    assert_eq!(
        adapter
            .handle_packet(
                &mut lodestone_world::World::new(),
                ConnectionState::Configuration,
                5,
                &[]
            )
            .expect("known packs"),
        vec![Directive::Send {
            packet_id: 6,
            payload: vec![7],
        }]
    );
    assert_eq!(
        adapter
            .handle_packet(
                &mut lodestone_world::World::new(),
                ConnectionState::Configuration,
                9,
                &[]
            )
            .expect("code of conduct"),
        vec![Directive::Send {
            packet_id: 10,
            payload: vec![],
        }]
    );
    assert_eq!(
        adapter
            .handle_packet(
                &mut lodestone_world::World::new(),
                ConnectionState::Configuration,
                11,
                &[]
            )
            .expect("finish configuration"),
        vec![
            Directive::Send {
                packet_id: 12,
                payload: vec![],
            },
            Directive::SetState(ConnectionState::Play),
        ]
    );
    assert_eq!(
        adapter
            .encode_action(
                ConnectionState::Play,
                &ClientAction::KeepAliveResponse { id: 42 }
            )
            .expect("encode succeeds"),
        Some((8, vec![4, 5, 6]))
    );
}

#[test]
fn adapter_can_express_login_directly_to_play_without_configuration_or_compression() {
    #[derive(Debug)]
    struct LegacyLoginAdapter;

    impl VersionAdapter for LegacyLoginAdapter {
        fn protocol_version(&self) -> i32 {
            1
        }

        fn minecraft_versions(&self) -> &'static [&'static str] {
            &["legacy-test"]
        }

        fn supports(&self, protocol: i32) -> bool {
            protocol == self.protocol_version()
        }

        fn begin_login(
            &self,
            _profile: &LoginProfile,
            _server: &ServerAddress,
        ) -> Result<Vec<Directive>, AdapterError> {
            Ok(vec![
                Directive::Send {
                    packet_id: 0,
                    payload: vec![1],
                },
                Directive::Send {
                    packet_id: 1,
                    payload: vec![2],
                },
            ])
        }

        fn handle_packet(
            &self,
            _world: &mut dyn lodestone_world::WorldSink,
            state: ConnectionState,
            packet_id: i32,
            _payload: &[u8],
        ) -> Result<Vec<Directive>, AdapterError> {
            Ok(match (state, packet_id) {
                (ConnectionState::Login, 2) => vec![Directive::SetState(ConnectionState::Play)],
                _ => Vec::new(),
            })
        }

        fn encode_action(
            &self,
            _state: ConnectionState,
            _action: &ClientAction,
        ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
            Ok(None)
        }
    }

    let adapter = LegacyLoginAdapter;
    let directives = [
        adapter
            .begin_login(
                &LoginProfile {
                    username: "Alex".to_owned(),
                    uuid: Uuid::from_u128(1),
                },
                &ServerAddress {
                    host: "localhost".to_owned(),
                    port: 25565,
                },
            )
            .expect("begin login"),
        adapter
            .handle_packet(
                &mut lodestone_world::World::new(),
                ConnectionState::Login,
                2,
                &[],
            )
            .expect("login finished"),
    ]
    .concat();

    assert!(
        !directives
            .iter()
            .any(|directive| matches!(directive, Directive::SetCompression(_)))
    );
    assert!(!directives.iter().any(|directive| matches!(
        directive,
        Directive::SetState(ConnectionState::Configuration)
    )));
    assert_eq!(
        directives.last(),
        Some(&Directive::SetState(ConnectionState::Play))
    );
}

#[test]
fn empty_directive_batch_represents_an_ignored_packet() {
    #[derive(Debug)]
    struct IgnoringAdapter;

    impl VersionAdapter for IgnoringAdapter {
        fn protocol_version(&self) -> i32 {
            3
        }

        fn minecraft_versions(&self) -> &'static [&'static str] {
            &["ignore-test"]
        }

        fn supports(&self, protocol: i32) -> bool {
            protocol == self.protocol_version()
        }

        fn begin_login(
            &self,
            _profile: &LoginProfile,
            _server: &ServerAddress,
        ) -> Result<Vec<Directive>, AdapterError> {
            Ok(Vec::new())
        }

        fn handle_packet(
            &self,
            _world: &mut dyn lodestone_world::WorldSink,
            _state: ConnectionState,
            _packet_id: i32,
            _payload: &[u8],
        ) -> Result<Vec<Directive>, AdapterError> {
            Ok(Vec::new())
        }

        fn encode_action(
            &self,
            _state: ConnectionState,
            _action: &ClientAction,
        ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
            Ok(None)
        }
    }

    assert_eq!(
        IgnoringAdapter
            .handle_packet(
                &mut lodestone_world::World::new(),
                ConnectionState::Configuration,
                99,
                &[1, 2, 3]
            )
            .expect("ignored packet"),
        Vec::new()
    );
}

#[test]
fn directive_ordering_is_preserved_within_a_batch() {
    let directives = [
        Directive::Send {
            packet_id: 7,
            payload: vec![1, 2, 3],
        },
        Directive::SetState(ConnectionState::Configuration),
    ];

    assert!(matches!(directives[0], Directive::Send { .. }));
    assert_eq!(
        directives[1],
        Directive::SetState(ConnectionState::Configuration)
    );
}

#[test]
fn connection_state_is_reexported_lodestone_core_state() {
    fn accepts_model_state(state: ConnectionState) -> Directive {
        Directive::SetState(state)
    }

    let core_state = lodestone_core::State::Configuration;

    assert_eq!(
        accepts_model_state(core_state),
        Directive::SetState(ConnectionState::Configuration)
    );
}

#[test]
fn unsupported_adapter_error_is_distinct_from_decode_failure() {
    let error = AdapterError::Unsupported("online-mode authentication".to_owned());

    assert_eq!(
        error.to_string(),
        "unsupported protocol feature: online-mode authentication"
    );
    assert_eq!(
        error.clone(),
        AdapterError::Unsupported("online-mode authentication".to_owned())
    );
    assert!(
        matches!(error, AdapterError::Unsupported(message) if message == "online-mode authentication")
    );
    assert!(!matches!(
        AdapterError::Unsupported("online-mode authentication".to_owned()),
        AdapterError::Decode(_)
    ));
}

#[test]
fn client_event_carriers_cover_play_state_gameplay_packets_without_protocol_ids() {
    let sound = ResourceKey::from_str("minecraft:block.note_block.pling").expect("sound key");
    let particle = ResourceKey::from_str("minecraft:happy_villager").expect("particle key");
    let menu = ResourceKey::from_str("minecraft:generic_9x3").expect("menu key");
    let item = ItemStack {
        item: ResourceKey::from_str("minecraft:stone").expect("item key"),
        count: 64,
        components: ItemComponents::default(),
    };

    let events = [
        ClientEvent::WeatherChanged {
            raining: Some(true),
            rain_level: Some(0.75),
            thunder_level: None,
        },
        ClientEvent::GameModeChanged {
            game_mode: GameMode::Creative,
        },
        ClientEvent::SpawnPositionChanged {
            dimension: ResourceKey::from_str("minecraft:overworld").expect("dimension key"),
            pos: BlockPos::new(12, 64, -5),
            angle: 90.0,
            pitch: 15.0,
        },
        ClientEvent::AbilitiesChanged {
            invulnerable: true,
            flying: false,
            can_fly: true,
            instabuild: true,
            flying_speed: 0.05,
            walking_speed: 0.1,
        },
        ClientEvent::Sound {
            sound: sound.clone(),
            category: SoundCategory::Block,
            pos: Vec3::new(1.0, 2.0, 3.0),
            volume: 1.0,
            pitch: 0.8,
            fixed_range: Some(32.0),
            seed: 123,
        },
        ClientEvent::EntitySound {
            sound,
            category: SoundCategory::Ui,
            entity_id: 42,
            volume: 0.5,
            pitch: 1.2,
            fixed_range: None,
            seed: 456,
        },
        ClientEvent::LevelEvent {
            event: 1023,
            pos: BlockPos::new(1, 2, 3),
            data: 7,
            global: false,
        },
        ClientEvent::Particles {
            particle,
            long_distance: true,
            pos: Vec3::new(4.0, 5.0, 6.0),
            offset: Vec3f::new(0.1, 0.2, 0.3),
            max_speed: 0.4,
            count: 8,
        },
        ClientEvent::ContainerContent {
            window_id: 1,
            state_id: 9,
            items: vec![Some(item.clone()), None],
            carried_item: Some(item.clone()),
        },
        ClientEvent::ContainerSlot {
            window_id: 1,
            state_id: 10,
            slot: 5,
            item: Some(item.clone()),
        },
        ClientEvent::EntityEquipmentUpdated {
            entity_id: 42,
            equipment: vec![
                EntityEquipment {
                    slot: EquipmentSlot::MainHand,
                    item: Some(item),
                },
                EntityEquipment {
                    slot: EquipmentSlot::Saddle,
                    item: None,
                },
            ],
        },
        ClientEvent::ContainerData {
            window_id: 1,
            property: 2,
            value: 42,
        },
        ClientEvent::ScreenClosed { window_id: 1 },
        ClientEvent::ScreenOpened {
            window_id: 1,
            menu_type: menu,
            title: Text::literal("Chest"),
        },
    ];

    assert_eq!(events.len(), 14);
    assert!(matches!(
        &events[8],
        ClientEvent::ContainerContent {
            items,
            carried_item: Some(ItemStack { count: 64, .. }),
            ..
        } if items.len() == 2
    ));
    assert!(matches!(
        &events[10],
        ClientEvent::EntityEquipmentUpdated {
            equipment,
            ..
        } if equipment.len() == 2
            && equipment[0].slot == EquipmentSlot::MainHand
            && equipment[1].slot == EquipmentSlot::Saddle
            && equipment[1].item.is_none()
    ));
    assert!(matches!(
        events[11],
        ClientEvent::ContainerData {
            window_id: 1,
            property: 2,
            value: 42,
        }
    ));
    assert!(matches!(
        events[12],
        ClientEvent::ScreenClosed { window_id: 1 }
    ));
}

#[test]
fn sound_categories_match_vanilla_source_order_including_ui() {
    assert_eq!(
        SoundCategory::ALL,
        [
            SoundCategory::Master,
            SoundCategory::Music,
            SoundCategory::Record,
            SoundCategory::Weather,
            SoundCategory::Block,
            SoundCategory::Hostile,
            SoundCategory::Neutral,
            SoundCategory::Player,
            SoundCategory::Ambient,
            SoundCategory::Voice,
            SoundCategory::Ui,
        ]
    );

    for (ordinal, category) in SoundCategory::ALL.iter().copied().enumerate() {
        assert_eq!(SoundCategory::from_ordinal(ordinal as u8), Some(category));
        assert_eq!(category.ordinal(), ordinal as u8);
    }
    assert_eq!(SoundCategory::from_ordinal(11), None);
}

#[test]
fn equipment_slots_match_vanilla_set_equipment_order() {
    assert_eq!(
        EquipmentSlot::ALL,
        [
            EquipmentSlot::MainHand,
            EquipmentSlot::OffHand,
            EquipmentSlot::Feet,
            EquipmentSlot::Legs,
            EquipmentSlot::Chest,
            EquipmentSlot::Head,
            EquipmentSlot::Body,
            EquipmentSlot::Saddle,
        ]
    );

    for (ordinal, slot) in EquipmentSlot::ALL.iter().copied().enumerate() {
        assert_eq!(EquipmentSlot::from_ordinal(ordinal as u8), Some(slot));
        assert_eq!(slot.ordinal(), ordinal as u8);
    }
    assert_eq!(EquipmentSlot::from_ordinal(8), None);
}

#[test]
fn client_actions_cover_modern_play_interactions_without_protocol_ids() {
    let stone = ItemStack {
        item: ResourceKey::from_str("minecraft:stone").expect("item key"),
        count: 32,
        components: ItemComponents::default(),
    };
    let stick = ItemStack {
        item: ResourceKey::from_str("minecraft:stick").expect("item key"),
        count: 1,
        components: ItemComponents::default(),
    };

    let actions = vec![
        ClientAction::BlockAction {
            action: BlockActionKind::StartDestroy,
            pos: BlockPos::new(1, 64, -2),
            face: BlockFace::North,
            sequence: 41,
        },
        ClientAction::DropSelectedItem,
        ClientAction::DropSelectedItemStack,
        ClientAction::ChatAck { offset: 65 },
        ClientAction::SwapItemWithOffhand,
        ClientAction::ReleaseUseItem,
        ClientAction::Stab,
        ClientAction::UseItemOn {
            hand: Hand::Main,
            pos: BlockPos::new(2, 65, -3),
            face: BlockFace::Up,
            cursor: Vec3f::new(0.5, 1.0, 0.25),
            inside_block: false,
            sequence: 42,
        },
        ClientAction::UseItem {
            hand: Hand::Off,
            rotation: Rotation::new(90.0, 15.0),
            sequence: 43,
        },
        ClientAction::InteractEntity {
            entity_id: 7,
            interaction: EntityInteraction::InteractAt {
                hand: Hand::Main,
                target: Vec3::new(0.0, 1.25, 0.0),
            },
            sneaking: true,
        },
        ClientAction::InteractEntity {
            entity_id: 8,
            interaction: EntityInteraction::Attack,
            sneaking: false,
        },
        ClientAction::ContainerClick {
            window_id: 1,
            state_id: 9,
            slot: 5,
            button: 0,
            click_type: ContainerClickType::Pickup,
            changed_slots: vec![ContainerSlotChange {
                slot: 5,
                item: Some(stone.clone()),
            }],
            carried_item: Some(stone.clone()),
        },
        ClientAction::ContainerClose { window_id: 1 },
        ClientAction::SetCarriedItem { slot: 3 },
        ClientAction::SetCreativeModeSlot {
            slot: 36,
            item: Some(stone),
        },
        ClientAction::SetPlayerInput(PlayerInput {
            forward: true,
            backward: false,
            left: false,
            right: true,
            jump: true,
            shift: false,
            sprint: true,
        }),
        ClientAction::PlayerCommand {
            entity_id: 7,
            command: PlayerCommand::StartRidingJump { boost: 80 },
        },
        ClientAction::SetCreativeModeSlot {
            slot: -1,
            item: Some(stick),
        },
    ];

    assert_eq!(actions.len(), 18);
    assert!(matches!(
        &actions[0],
        ClientAction::BlockAction {
            action: BlockActionKind::StartDestroy,
            face: BlockFace::North,
            sequence: 41,
            ..
        }
    ));
    assert!(matches!(
        &actions[7],
        ClientAction::UseItemOn {
            sequence: 42,
            cursor,
            ..
        } if *cursor == Vec3f::new(0.5, 1.0, 0.25)
    ));
    assert!(matches!(
        &actions[11],
        ClientAction::ContainerClick {
            click_type: ContainerClickType::Pickup,
            changed_slots,
            carried_item: Some(ItemStack { count: 32, .. }),
            ..
        } if changed_slots.len() == 1
    ));
    assert_eq!(
        ClientActionKind::from(&actions[0]),
        ClientActionKind::BlockAction
    );
    assert_eq!(
        ClientActionKind::from(&actions[11]),
        ClientActionKind::ContainerClick
    );
    assert_eq!(
        ClientActionKind::from(&actions[15]),
        ClientActionKind::SetPlayerInput
    );
    assert_eq!(
        ClientActionKind::from(&actions[3]),
        ClientActionKind::ChatAck
    );
    assert_eq!(PlayerInput::default(), PlayerInput::EMPTY);
}

#[test]
fn client_event_carriers_cover_scoreboard_teams_and_boss_bars() {
    let team_params = TeamParameters {
        display_name: Text::literal("Builders"),
        prefix: Text::literal("[B] "),
        suffix: Text::literal("!"),
        name_tag_visibility: Visibility::HideForOtherTeams,
        collision_rule: CollisionRule::PushOwnTeam,
        color: Some(TeamColor::Gold),
        friendly_fire: true,
        see_friendly_invisibles: false,
    };
    let boss_id = Uuid::from_u128(0x12345678123456781234567812345678);

    let events = [
        ClientEvent::ObjectiveUpdate {
            name: "kills".to_owned(),
            mode: ObjectiveMode::Add,
            display_name: Some(Text::literal("Kills")),
            render_type: Some(ObjectiveRenderType::Integer),
            number_format: Some(NumberFormat::Styled(TextColor::Red)),
        },
        ClientEvent::DisplayObjective {
            slot: DisplaySlot::TeamSidebar(TeamColor::Gold),
            objective: Some("kills".to_owned()),
        },
        ClientEvent::ScoreUpdate {
            holder: "Steve".to_owned(),
            objective: "kills".to_owned(),
            value: 3,
            display: Some(Text::literal("Steve")),
            number_format: Some(NumberFormat::Fixed(Box::new(Text::literal("three")))),
        },
        ClientEvent::ScoreReset {
            holder: "Steve".to_owned(),
            objective: None,
        },
        ClientEvent::TeamUpdate {
            name: "builders".to_owned(),
            action: TeamAction::Create {
                params: Box::new(team_params.clone()),
                members: vec!["Steve".to_owned(), "Alex".to_owned()],
            },
        },
        ClientEvent::TeamUpdate {
            name: "builders".to_owned(),
            action: TeamAction::Update {
                params: Box::new(team_params),
            },
        },
        ClientEvent::BossBarUpdate {
            id: boss_id,
            action: BossAction::Add {
                title: Box::new(Text::literal("Dragon")),
                progress: 0.75,
                color: BossColor::Purple,
                overlay: BossOverlay::Notched10,
                darken: true,
                music: true,
                fog: false,
            },
        },
        ClientEvent::BossBarUpdate {
            id: boss_id,
            action: BossAction::UpdateFlags {
                darken: false,
                music: false,
                fog: true,
            },
        },
    ];

    assert_eq!(events.len(), 8);
    assert!(matches!(
        &events[0],
        ClientEvent::ObjectiveUpdate {
            mode: ObjectiveMode::Add,
            render_type: Some(ObjectiveRenderType::Integer),
            number_format: Some(NumberFormat::Styled(TextColor::Red)),
            ..
        }
    ));
    assert!(matches!(
        &events[4],
        ClientEvent::TeamUpdate {
            action: TeamAction::Create { members, params },
            ..
        } if members.len() == 2
            && params.color == Some(TeamColor::Gold)
            && params.name_tag_visibility == Visibility::HideForOtherTeams
    ));
    assert!(matches!(
        &events[6],
        ClientEvent::BossBarUpdate {
            id,
            action: BossAction::Add {
                progress,
                color: BossColor::Purple,
                overlay: BossOverlay::Notched10,
                darken: true,
                music: true,
                fog: false,
                ..
            },
        } if *id == boss_id && (*progress - 0.75).abs() < f32::EPSILON
    ));
}

/// Real "Notch joined the game" broadcast captured live from a 1.8.9 server
/// (protocol 47) as a raw clientbound chat payload: a VarInt-prefixed JSON
/// string followed by a position byte. Regenerated by the client crate's
/// `capture_real_chat_components` tool against the running containers.
const JOIN_JSON_PAYLOAD: &[u8] = include_bytes!("../tests/data/join_1_8_json.bin");

/// The same broadcast captured live from a modern server (protocol 776) as a
/// raw clientbound chat payload leading with the network-NBT text component.
const JOIN_NBT_PAYLOAD: &[u8] = include_bytes!("../tests/data/join_modern_nbt.bin");

#[test]
fn cross_format_oracle_flattens_real_json_and_nbt_identically() {
    // 1.8 payload: VarInt-length JSON string, then a trailing position byte.
    let mut json_reader = lodestone_core::Reader::new(JOIN_JSON_PAYLOAD);
    let json = json_reader
        .string(usize::MAX)
        .expect("captured 1.8 chat payload begins with a JSON string");
    let from_json = Text::from_json(&json);

    // Modern payload: leads with the network NBT text component.
    let mut nbt_reader = lodestone_core::Reader::new(JOIN_NBT_PAYLOAD);
    let nbt = lodestone_core::read_network_nbt(&mut nbt_reader)
        .expect("captured modern chat payload begins with an NBT component");
    let from_nbt = Text::from_nbt(&nbt);

    // The heart of F3: two serializations of the same logical message flatten to
    // the same plain text through one shared set of tree operations.
    assert_eq!(from_json.to_plain_string(), "Notch joined the game");
    assert_eq!(from_nbt.to_plain_string(), "Notch joined the game");
    assert_eq!(from_json.to_plain_string(), from_nbt.to_plain_string());
}

#[test]
fn cross_format_oracle_agrees_on_translate_structure() {
    // Beyond the flattened string, both front-ends must recover the *same tree*:
    // a `multiplayer.player.joined` translate with the player name as its single
    // argument. This is what makes the plain-text agreement non-coincidental.
    let mut json_reader = lodestone_core::Reader::new(JOIN_JSON_PAYLOAD);
    let json = json_reader.string(usize::MAX).unwrap();
    let from_json = Text::from_json(&json);

    let mut nbt_reader = lodestone_core::Reader::new(JOIN_NBT_PAYLOAD);
    let nbt = lodestone_core::read_network_nbt(&mut nbt_reader).unwrap();
    let from_nbt = Text::from_nbt(&nbt);

    for text in [&from_json, &from_nbt] {
        match &text.content {
            TextContent::Translate { key, with, .. } => {
                assert_eq!(key, "multiplayer.player.joined");
                assert_eq!(with.len(), 1);
                assert_eq!(with[0].to_plain_string(), "Notch");
            }
            other => panic!("expected a translate component, got {other:?}"),
        }
    }
}

#[test]
fn path_type_malus_matches_vanilla() {
    assert_eq!(PathType::Open.malus(), 0.0);
    assert_eq!(PathType::Blocked.malus(), -1.0);
    assert_eq!(PathType::Fence.malus(), -1.0);
    assert_eq!(PathType::Water.malus(), 8.0);
    assert_eq!(PathType::Fire.malus(), 16.0);
    assert_eq!(PathType::Breach.malus(), 4.0);
    // Passable types have a non-negative malus; impassable ones are -1.
    assert!(PathType::Rail.malus() >= 0.0);
    assert!(PathType::Lava.malus() < 0.0);
}

#[test]
fn path_type_registry_resolves_ids() {
    struct Table(Vec<PathType>);
    impl PathTypeRegistry for Table {
        fn path_type(&self, id: u32) -> Option<PathType> {
            self.0.get(id as usize).copied()
        }
        fn state_count(&self) -> u32 {
            self.0.len() as u32
        }
    }
    let t = Table(vec![PathType::Open, PathType::Blocked]);
    assert_eq!(t.path_type(0), Some(PathType::Open));
    assert_eq!(t.path_type(1), Some(PathType::Blocked));
    assert_eq!(t.path_type(2), None);
    assert_eq!(t.state_count(), 2);
}

// ---------------------------------------------------------------------------
// Legacy `§` expansion — `StringDecomposer.iterateFormatted` parity
// ---------------------------------------------------------------------------

/// The fixture every legacy-expansion gate below shares: a colour, then a
/// formatting code, then a **second** colour (which must clear the formatting the
/// first one turned on), then `§r`, then an **unrecognised** code.
///
/// Every discriminating property this pair of tests can check is present in this
/// one string, and each is there because the wrong hypothesis is plausible:
/// `§a` clearing `§l` distinguishes "a colour resets formatting" from the
/// backwards reading; `§r` distinguishes reset-to-nothing from vanilla's
/// reset-to-the-enclosing-component's-style; and `§q` distinguishes dropping both
/// characters from the two other plausible answers (print the pair, or drop only
/// the `§`).
const LEGACY_FIXTURE: &str = "§cred §lbold §agreen§r plain §qdropped";

/// The visible text of [`LEGACY_FIXTURE`] once every code is consumed. `§q` and
/// its `§` are both gone; `dropped` is not.
const LEGACY_FIXTURE_VISIBLE: &str = "red bold green plain dropped";

/// A style with all five formatting flags set **deliberately alternating**, so
/// any swap of two adjacent flags anywhere between here and a span changes an
/// assertion. Two equal booleans coincide half the time by chance and cannot
/// detect a transposition at all.
fn alternating_style() -> TextStyle {
    TextStyle {
        font: None,
        color: Some(TextColor::Gold),
        bold: Some(true),
        italic: Some(false),
        underlined: Some(true),
        strikethrough: Some(false),
        obfuscated: Some(true),
    }
}

/// `Text::from_legacy` + `to_spans` on a bare legacy string: the span **count**
/// is predicted as well as the styles, because a decomposer that drops a span
/// boundary still produces plausible-looking styles.
#[test]
fn legacy_codes_decompose_to_four_spans_with_vanilla_reset_semantics() {
    let spans = Text::from_legacy(LEGACY_FIXTURE).to_spans();

    // Four, not five: `§q` contributes no boundary because vanilla's
    // unconditional `i++` swallows an unrecognised pair whole.
    assert_eq!(spans.len(), 4, "span boundaries: {spans:?}");
    assert_eq!(
        spans.iter().map(|s| s.text.as_str()).collect::<String>(),
        LEGACY_FIXTURE_VISIBLE
    );

    assert_eq!(spans[0].text, "red ");
    assert_eq!(spans[0].style.color, Some(TextColor::Red));
    assert_eq!(spans[0].style.bold, Some(false));

    // `§l` adds bold and leaves the colour alone.
    assert_eq!(spans[1].text, "bold ");
    assert_eq!(spans[1].style.color, Some(TextColor::Red));
    assert_eq!(spans[1].style.bold, Some(true));

    // `§a` is a colour, so it clears the bold `§l` turned on. Getting this
    // backwards is what makes `§c§lFoo` render almost-right.
    assert_eq!(spans[2].text, "green");
    assert_eq!(spans[2].style.color, Some(TextColor::Green));
    assert_eq!(spans[2].style.bold, Some(false));

    // `§r` at the root, where there is no enclosing style, is `Style.EMPTY` —
    // and specifically *no colour*, not white.
    assert_eq!(spans[3].text, " plain dropped");
    assert_eq!(spans[3].style, TextStyle::default());
    assert_eq!(spans[3].style.color, None);
}

/// The surface fix: codes living inside a **modern component's** literal content,
/// which is how a plugin server ships them. This is the shape that used to reach
/// the screen as `§7` glyphs.
///
/// The enclosing component carries [`alternating_style`], so the span `§r`
/// produces reads its five flags back — vanilla's `resetStyle` is seeded with the
/// component's own style, not `Style.EMPTY`, and reset-to-nothing would fail
/// here on all five.
#[test]
fn legacy_codes_inside_a_component_expand_and_inherit_the_enclosing_style() {
    let mut node = Text::literal(LEGACY_FIXTURE);
    node.style = alternating_style();
    let spans = node.to_spans();

    assert_eq!(spans.len(), 4, "span boundaries: {spans:?}");
    assert!(
        spans.iter().all(|s| !s.text.contains(text::LEGACY_PREFIX)),
        "a § survived expansion: {spans:?}"
    );

    // A colour code clears the *enclosing* component's formatting too — all
    // three of the component's `true` flags are off inside this run.
    assert_eq!(spans[0].text, "red ");
    assert_eq!(spans[0].style.color, Some(TextColor::Red));
    assert_eq!(spans[0].style.bold, Some(false));
    assert_eq!(spans[0].style.underlined, Some(false));
    assert_eq!(spans[0].style.obfuscated, Some(false));

    assert_eq!(spans[1].style.bold, Some(true));
    assert_eq!(spans[1].style.underlined, Some(false));

    assert_eq!(spans[2].style.color, Some(TextColor::Green));
    assert_eq!(spans[2].style.bold, Some(false));

    // `§r` restores the component's style, flag for flag. Read back
    // individually: a swap of two adjacent flags anywhere in `inherit` or in
    // the expansion pass moves one of these five off its expected value.
    assert_eq!(spans[3].text, " plain dropped");
    assert_eq!(spans[3].style.color, Some(TextColor::Gold));
    assert_eq!(spans[3].style.bold, Some(true));
    assert_eq!(spans[3].style.italic, Some(false));
    assert_eq!(spans[3].style.underlined, Some(true));
    assert_eq!(spans[3].style.strikethrough, Some(false));
    assert_eq!(spans[3].style.obfuscated, Some(true));
}

/// The two flattening functions really are different functions. Without this,
/// `to_spans_ignoring_legacy_codes` silently becoming an alias for `to_spans`
/// would leave every assertion above passing while the long name stopped meaning
/// anything — and the guard test that forbids the long name at a render surface
/// would then be forbidding nothing.
#[test]
fn the_non_expanding_flatten_really_leaves_the_codes_in_place() {
    let mut node = Text::literal(LEGACY_FIXTURE);
    node.style = alternating_style();

    let raw = node.to_spans_ignoring_legacy_codes();
    assert_eq!(raw.len(), 1, "no expansion means no extra boundaries: {raw:?}");
    assert_eq!(raw[0].text, LEGACY_FIXTURE);
    assert_eq!(raw[0].style, alternating_style());
}

/// `§x§r§r§g§g§b§b` — the BungeeCord hex dialect — is **not** honoured by vanilla
/// 26.2, and must not be honoured here either. `getByCode('x')` is null, so `§x`
/// is dropped whole and the six pairs after it are read as six ordinary colour
/// codes; the run ends up coloured by the last of them.
#[test]
fn the_bungeecord_hex_dialect_resolves_to_its_last_digit_not_to_a_hex_colour() {
    // `§x` then `ff00aa` written the dialect's way.
    let spans = Text::from_legacy("§x§f§f§0§0§a§ahex").to_spans();

    assert_eq!(spans.len(), 1, "only the final code precedes any text: {spans:?}");
    assert_eq!(spans[0].text, "hex");
    // `§a` — green. *Not* `Rgb(0xff00aa)`.
    assert_eq!(spans[0].style.color, Some(TextColor::Green));
}

/// A dangling `§` is dropped, not drawn. Vanilla `break`s out of
/// `iterateFormatted` without feeding it to the sink.
#[test]
fn a_dangling_section_sign_is_dropped() {
    let spans = Text::from_legacy("tail§").to_spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].text, "tail");
}
