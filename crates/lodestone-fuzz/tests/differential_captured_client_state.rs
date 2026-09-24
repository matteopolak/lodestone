//! Replay of a small server-authored client-state packet script.
//!
//! The packet payloads are captured fixtures, while the expected item identity
//! and count come from the fixture annotations rather than from a re-encoder.
#![cfg(feature = "v26-2")]

use lodestone_client::{
    ClientBuilder, ClientEvent, ConnectionState, Directive, EventStream, LoginProfile,
    ServerAddress, VersionAdapter,
};
use lodestone_fuzz::{read_hex_fixture, v26_2_fixture_path};
use lodestone_model::item::{
    ItemComponents, ItemTool, MobEffectInstance, ToolBlocks, ToolPatch, ToolRule,
};
use lodestone_model::{AdapterError, ClientAction, WorldSink};
use lodestone_net::{Connection, memory_pair};
use lodestone_v26_2::{V770Adapter, packet_ids::play};
use tokio::io::DuplexStream;
use tokio::runtime::{Builder, Runtime};
use uuid::Uuid;

const SLOT: usize = 36;

#[derive(Clone, Copy)]
struct CapturedStep {
    fixture: &'static str,
    expected_item: &'static str,
    expected_count: i32,
}

fn captured_inventory_script() -> [CapturedStep; 2] {
    [
        CapturedStep {
            fixture: "tool_component_explicit.hex",
            expected_item: "minecraft:diamond_pickaxe",
            expected_count: 1,
        },
        CapturedStep {
            fixture: "potion_contents_complete.hex",
            expected_item: "minecraft:potion",
            expected_count: 1,
        },
    ]
}

/// Expected patch fields come from the capture's source command and annotated
/// bytes, independent of both adapter decoding and client inventory folding.
fn expected_components(fixture: &str) -> ItemComponents {
    let mut components = ItemComponents::default();
    match fixture {
        "tool_component_explicit.hex" => {
            components.tool = ToolPatch::Set(ItemTool::new(
                vec![
                    ToolRule::new(
                        ToolBlocks::Tag("minecraft:incorrect_for_diamond_tool".parse().unwrap()),
                        None,
                        Some(false),
                    ),
                    ToolRule::new(ToolBlocks::Blocks(vec![1, 193]), Some(12.5), None),
                ],
                1.5,
                3,
                true,
            ));
        }
        "tool_component_absent_plain_pickaxe.hex" => {}
        "potion_contents_complete.hex" => {
            components.potion = Some(13);
            components.potion_color = Some(0xff12_3456);
            components.potion_custom_name = Some("night_vision".into());
            components.potion_custom_effects = vec![MobEffectInstance {
                effect_id: 18,
                amplifier: 1,
                duration_ticks: 45,
                ambient: false,
                show_particles: true,
                show_icon: true,
            }];
        }
        other => panic!("no independent component expectation for {other}"),
    }
    components
}

fn component_difference(
    actual: &ItemComponents,
    expected: &ItemComponents,
) -> Option<&'static str> {
    if actual.tool != expected.tool {
        Some("tool")
    } else if actual.potion != expected.potion {
        Some("potion")
    } else if actual.potion_color != expected.potion_color {
        Some("potion_color")
    } else if actual.potion_custom_name != expected.potion_custom_name {
        Some("potion_custom_name")
    } else if actual.potion_custom_effects != expected.potion_custom_effects {
        Some("potion_custom_effects")
    } else if actual.has_unmodeled != expected.has_unmodeled {
        Some("has_unmodeled")
    } else {
        None
    }
}

/// Starts the normal client driver in Play without needing a second protocol
/// implementation for login. Packet decoding still runs through the real
/// 26.2 adapter; the synthetic state transition only keeps this corpus test
/// focused on the captured Play packets.
#[derive(Debug)]
struct CapturedAdapter(V770Adapter);

impl VersionAdapter for CapturedAdapter {
    fn protocol_version(&self) -> i32 {
        self.0.protocol_version()
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        self.0.minecraft_versions()
    }

    fn supports(&self, protocol: i32) -> bool {
        self.0.supports(protocol)
    }

    fn begin_login(
        &self,
        _profile: &LoginProfile,
        _server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        Ok(vec![Directive::SetState(ConnectionState::Play)])
    }

    fn handle_packet(
        &self,
        world: &mut dyn WorldSink,
        state: ConnectionState,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        self.0.handle_packet(world, state, packet_id, payload)
    }

    fn encode_action(
        &self,
        state: ConnectionState,
        action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        self.0.encode_action(state, action)
    }
}

struct CapturedClient {
    handle: lodestone_client::ClientHandle,
    events: EventStream,
    peer: Connection<DuplexStream>,
    runtime: Runtime,
}

impl CapturedClient {
    fn new() -> Self {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("captured client runtime");
        let (client_io, server_io) = memory_pair();
        let (handle, events) = runtime.block_on(async {
            ClientBuilder::new(
                ServerAddress {
                    host: "captured-26-2".into(),
                    port: 0,
                },
                LoginProfile {
                    username: "CapturedFixture".into(),
                    uuid: Uuid::nil(),
                },
                Box::new(CapturedAdapter(V770Adapter::new())),
            )
            .connect_with(client_io)
        });
        Self {
            handle,
            events,
            peer: Connection::new(server_io),
            runtime,
        }
    }

    fn replay(&mut self, fixture: &str) -> Result<(), String> {
        let payload = read_hex_fixture(&v26_2_fixture_path(fixture));
        self.runtime.block_on(async {
            self.peer
                .write_packet(play::clientbound::CONTAINER_SET_SLOT, &payload)
                .await
                .map_err(|error| format!("write captured packet: {error}"))?;
            match self.events.recv().await {
                Some(ClientEvent::ContainerSlot { .. }) => Ok(()),
                Some(event) => Err(format!("captured packet emitted the wrong event: {event:?}")),
                None => Err("client driver ended before the captured packet event".into()),
            }
        })
    }

    fn slot_snapshot(&self) -> Option<(String, i32)> {
        self.handle.player_menu().slot_item(SLOT).map(|item| {
            (item.item().to_string(), item.count())
        })
    }
}

impl Drop for CapturedClient {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

#[test]
fn captured_inventory_script_reaches_client_state() {
    let script = captured_inventory_script();
    let mut client = CapturedClient::new();
    for step in script {
        client.replay(step.fixture).expect("captured packet replay");
        assert_eq!(
            client.slot_snapshot(),
            Some((step.expected_item.into(), step.expected_count)),
            "captured fixture {} must reach the public inventory read model",
            step.fixture
        );
    }
}

#[test]
fn captured_inventory_control_reports_a_wrong_expected_value() {
    let step = captured_inventory_script()[0];
    let mut client = CapturedClient::new();
    client.replay(step.fixture).expect("captured packet replay");
    let actual = client.slot_snapshot().expect("captured item in target slot");
    assert_ne!(
        actual,
        ("minecraft:stone".into(), step.expected_count),
        "the negative control must distinguish the captured item from a wrong oracle value"
    );
}

#[test]
fn captured_component_replacements_reach_client_state() {
    let mut client = CapturedClient::new();
    for fixture in [
        "tool_component_explicit.hex",
        "tool_component_absent_plain_pickaxe.hex",
        "potion_contents_complete.hex",
        "tool_component_absent_plain_pickaxe.hex",
    ] {
        client.replay(fixture).expect("captured replacement replay");
        let menu = client.handle.player_menu();
        let item = menu.slot_item(SLOT).expect("captured item in target slot");
        assert_eq!(
            component_difference(
                &lodestone_model::ItemStack::from(item).components,
                &expected_components(fixture),
            ),
            None,
            "fixture {fixture} must replace component state in public inventory slot {SLOT}"
        );
    }
}

#[test]
fn captured_component_controls_detect_stale_tool_and_potion_fields() {
    let mut client = CapturedClient::new();
    for fixture in ["tool_component_explicit.hex", "potion_contents_complete.hex"] {
        client.replay(fixture).expect("captured populated patch replay");
        let stale = lodestone_model::ItemStack::from(
            client.handle.player_menu().slot_item(SLOT).unwrap(),
        )
        .components;
        client
            .replay("tool_component_absent_plain_pickaxe.hex")
            .expect("plain replacement");
        let menu = client.handle.player_menu();
        let actual = lodestone_model::ItemStack::from(menu.slot_item(SLOT).unwrap()).components;
        assert_eq!(component_difference(&actual, &ItemComponents::default()), None);
        assert_eq!(
            component_difference(&actual, &stale),
            Some(if fixture == "tool_component_explicit.hex" {
                "tool"
            } else {
                "potion"
            }),
            "the detector must reject a stale component patch after {fixture} is replaced"
        );
    }
}
