//! Fixed release-jar witnesses for protocol metadata and packet IDs.

use lodestone_v26_3::{
    DATA_PACK_VERSION, DATA_VERSION, MINECRAFT_VERSION, PROTOCOL, RESOURCE_PACK_VERSION,
    connection_dialect, packet_ids,
};

#[test]
fn release_metadata_matches_embedded_version_json() {
    let version: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/version_26_3.json")).unwrap();
    assert_eq!(MINECRAFT_VERSION, version["id"].as_str().unwrap());
    assert_eq!(PROTOCOL as i64, version["protocol_version"].as_i64().unwrap());
    assert_eq!(DATA_VERSION as u64, version["world_version"].as_u64().unwrap());
    assert_eq!(
        DATA_PACK_VERSION,
        (
            version["pack_version"]["data_major"].as_u64().unwrap() as u32,
            version["pack_version"]["data_minor"].as_u64().unwrap() as u32,
        )
    );
    assert_eq!(
        RESOURCE_PACK_VERSION,
        (
            version["pack_version"]["resource_major"]
                .as_u64()
                .unwrap() as u32,
            version["pack_version"]["resource_minor"]
                .as_u64()
                .unwrap() as u32,
        )
    );
}

#[test]
fn report_packet_samples_resolve_both_ways() {
    for line in include_str!("fixtures/packet_id_samples_26_3.tsv").lines() {
        let mut fields = line.split('\t');
        let state = fields.next().unwrap().parse::<u8>().unwrap();
        let bound = fields.next().unwrap().parse::<u8>().unwrap();
        let name = fields.next().unwrap();
        let id = fields.next().unwrap().parse::<i32>().unwrap();
        assert!(fields.next().is_none());
        assert_eq!(packet_ids::id_for(state, bound, name), Some(id), "{name}");
        assert_eq!(packet_ids::name_for(state, bound, id), Some(name), "{name}");
    }
}

#[test]
fn additions_and_removal_are_not_silent_aliases() {
    assert_eq!(packet_ids::configuration::clientbound::ENTRIES.len(), 21);
    assert_eq!(packet_ids::play::clientbound::ENTRIES.len(), 144);
    assert_eq!(packet_ids::play::serverbound::ENTRIES.len(), 69);
    assert_eq!(
        packet_ids::id_for(
            packet_ids::STATE_PLAY,
            packet_ids::BOUND_SERVERBOUND,
            "minecraft:swing",
        ),
        None
    );
    assert_eq!(
        lodestone_v26_2::packet_ids::id_for(
            packet_ids::STATE_PLAY,
            packet_ids::BOUND_SERVERBOUND,
            "minecraft:punch",
        ),
        None
    );
}

#[test]
fn connection_dialect_uses_release_identity() {
    let dialect = connection_dialect();
    assert_eq!(dialect.protocol_version(), 777);
    assert_eq!(dialect.minecraft_versions(), &["26.3"]);
}
