//! Tests for end-gateway contact.

use super::*;
use lodestone_model::Vec3;
use crate::server::tests::default_block_state;

#[test]
fn gateway_contact_reads_generated_metadata_and_prefers_live_registry() {
    let gateway = BlockPos::new(12, 70, -4);
    let source = EndGatewaySource {
        state: default_block_state(crate::portal::END_GATEWAY_BLOCK),
        generated: Some((
            gateway,
            BlockEntity::EndGateway {
                exit: Some(BlockPos::new(100, 50, 0)),
                exact: true,
            },
        )),
    };
    let registry = BlockEntityHandle::new();

    assert_eq!(
        end_gateway_destination(&source, &registry, gateway),
        Some(Vec3::new(100.5, 50.0, 0.5)),
        "a generated gateway must consume its exact exit metadata"
    );

    registry.with(|entries| {
        entries.insert(
            gateway,
            BlockEntity::EndGateway {
                exit: Some(BlockPos::new(-20, 80, 30)),
                exact: true,
            },
        );
    });
    assert_eq!(
        end_gateway_destination(&source, &registry, gateway),
        Some(Vec3::new(-19.5, 80.0, 30.5)),
        "a live edited sidecar must outrank the generated snapshot"
    );

    let removed = EndGatewaySource {
        state: StateId::AIR,
        generated: source.generated.clone(),
    };
    assert_eq!(
        end_gateway_destination(&removed, &registry, gateway),
        None,
        "a stale sidecar must not teleport through a removed gateway block"
    );

    let missing_metadata = EndGatewaySource {
        state: default_block_state(crate::portal::END_GATEWAY_BLOCK),
        generated: None,
    };
    assert_eq!(
        end_gateway_destination(&missing_metadata, &BlockEntityHandle::new(), gateway),
        None,
        "a gateway without an exit must leave the player in place"
    );
}

#[test]
fn gateway_contact_is_end_player_only() {
    assert!(end_gateway_contact_allowed(
        crate::dimension::Dimension::End,
        true
    ));
    assert!(!end_gateway_contact_allowed(
        crate::dimension::Dimension::Overworld,
        true
    ));
    assert!(!end_gateway_contact_allowed(
        crate::dimension::Dimension::End,
        false
    ));
}

#[test]
fn gateway_contact_production_decision_updates_position_and_cooldown() {
    let gateway = BlockPos::new(12, 70, -4);
    let source = EndGatewaySource {
        state: default_block_state(crate::portal::END_GATEWAY_BLOCK),
        generated: Some((
            gateway,
            BlockEntity::EndGateway {
                exit: Some(BlockPos::new(100, 50, 0)),
                exact: true,
            },
        )),
    };
    let registry = BlockEntityHandle::new();

    let teleport = resolve_end_gateway_contact(
        &source,
        &registry,
        gateway,
        crate::dimension::Dimension::End,
        0,
        true,
        false,
    )
    .expect("the production contact seam must produce a visible teleport");
    assert_eq!(teleport.position, Vec3::new(100.5, 50.0, 0.5));
    assert_eq!(teleport.dimension, crate::dimension::Dimension::End);
    assert_eq!(teleport.cooldown, END_GATEWAY_CONTACT_COOLDOWN);

    assert!(resolve_end_gateway_contact(
        &source,
        &registry,
        gateway,
        crate::dimension::Dimension::End,
        END_GATEWAY_CONTACT_COOLDOWN,
        true,
        false,
    )
    .is_none());
    assert!(resolve_end_gateway_contact(
        &EndGatewaySource {
            state: default_block_state(crate::portal::END_GATEWAY_BLOCK),
            generated: None,
        },
        &registry,
        gateway,
        crate::dimension::Dimension::End,
        0,
        true,
        false,
    )
    .is_none());
    assert!(resolve_end_gateway_contact(
        &source,
        &registry,
        gateway,
        crate::dimension::Dimension::End,
        0,
        false,
        true,
    )
    .is_none());
}

struct EndGatewaySource {
    state: StateId,
    generated: Option<(BlockPos, BlockEntity)>,
}

impl ChunkSource for EndGatewaySource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 256)
    }

    fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
        self.state
    }

    fn block_entity(&self, x: i32, y: i32, z: i32) -> Option<BlockEntity> {
        self.generated.as_ref().and_then(|(pos, entity)| {
            (*pos == BlockPos::new(x, y, z)).then_some(entity.clone())
        })
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        crate::chunk::DEFAULT_BIOME.to_owned()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
}
