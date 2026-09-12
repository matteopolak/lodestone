//! The protocol seam between the version-free integrated server and a
//! version-specific packet format.
//!
//! [`ServerProtocol`] is the mirror of the client's `VersionAdapter`: it is the
//! **only** point where wire ids, encodings, NBT and registries enter the
//! server. A version/protocol crate implements it; this crate never names a
//! protocol number. Keeping the coupling behind one trait is what lets the
//! integrated-server loop stay shared while each version supplies its own
//! encoders/decoders (plan §3).

use url::Url;

mod codec;
mod packets;
mod session;

pub use codec::{ChunkEncodeError, ChunkEncoder, ServerProtocol, WorldgenScope};
pub use packets::{ServerBound, ServerDirective};
pub use session::{
    Abilities, BossBarSnapshot, EntitySnapshot, MerchantOfferOut, MetadataField, PlayerListing,
    ResourcePackPush,
};

/// The `EntityEvent` constants this crate sends through
/// [`ServerProtocol::encode_entity_event`], transcribed from
/// the real per-entity byte-event constant set.
///
/// A module rather than a Rust `enum`, because the wire field is an arbitrary
/// byte whose meaning is **per entity type**: the real engine reuses values across
/// classes (the entity-event packet carries no type tag), so an
/// exhaustive enum would claim a closed set that does not exist. Naming only
/// the values with a producer here keeps the set honest, and the constant name
/// keeps the number out of the call site.
pub mod entity_event {
    /// The real death-event constant — the living-entity die broadcast. The client's
    /// own entity-event handler's death case starts `deathTime`, which is
    /// what tips a mob onto its side and holds the death screen's red overlay.
    pub const DEATH: u8 = 3;

    /// The real taming-failed constant — the smoke puff of a failed tame roll.
    pub const TAMING_FAILED: u8 = 6;

    /// The real taming-succeeded constant — the hearts of a successful tame.
    pub const TAMING_SUCCEEDED: u8 = 7;

    /// The real in-love-hearts constant — the breeding hearts an animal in love
    /// emits. **Not** the villager's own love-hearts constant (12).
    pub const IN_LOVE_HEARTS: u8 = 18;
}

/// A valid resource-pack URL held by the server's version-free vocabulary.
///
/// The wrapper keeps the raw URL string behind an explicit parse boundary.
/// Resource-pack downloads are restricted to absolute `http` and `https`
/// URLs, matching the client-side admission rule; packet encoders still get a
/// borrowed wire spelling through [`Self::as_str`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResourcePackUrl(Url);

/// Why a resource-pack URL could not cross the server's version-free seam.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResourcePackUrlError {
    /// The input is not an absolute URL understood by the URL parser.
    #[error("{0}")]
    Invalid(url::ParseError),
    /// The URL uses a scheme other than `http` or `https`.
    #[error("unsupported resource-pack URL scheme {0:?}")]
    UnsupportedScheme(String),
}

impl ResourcePackUrl {
    /// Parses and validates a resource-pack URL before it crosses the
    /// version-free server seam.
    pub fn parse(raw: impl AsRef<str>) -> Result<Self, ResourcePackUrlError> {
        let url = Url::parse(raw.as_ref()).map_err(ResourcePackUrlError::Invalid)?;
        Self::try_from(url)
    }

    /// Borrows the canonical URL text for wire encoding and diagnostics.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Borrows the parsed URL when a caller needs URL semantics rather than
    /// its wire spelling.
    #[must_use]
    pub fn as_url(&self) -> &Url {
        &self.0
    }
}

impl std::fmt::Display for ResourcePackUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ResourcePackUrl {
    type Err = ResourcePackUrlError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

impl TryFrom<Url> for ResourcePackUrl {
    type Error = ResourcePackUrlError;

    fn try_from(url: Url) -> Result<Self, Self::Error> {
        if matches!(url.scheme(), "http" | "https") {
            Ok(Self(url))
        } else {
            Err(ResourcePackUrlError::UnsupportedScheme(
                url.scheme().to_owned(),
            ))
        }
    }
}

impl std::ops::Deref for ResourcePackUrl {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

#[cfg(test)]
mod tests {
    use lodestone_core::State;
    use lodestone_model::{
        Difficulty, EntityAttributeSnapshot, GameMode, ItemStack, ResourceKey, Rotation, Vec3,
    };
    use uuid::Uuid;

    use crate::chunk::ChunkColumn;

    use super::*;

    #[test]
    fn resource_pack_url_is_parsed_at_the_server_seam() {
        let url = ResourcePackUrl::parse("https://example.invalid/packs/demo.zip")
            .expect("well-formed resource-pack URL");
        assert_eq!(url.as_url().scheme(), "https");
        assert_eq!(url.as_str(), "https://example.invalid/packs/demo.zip");
        assert_eq!(&*url, url.as_str());
        assert!(ResourcePackUrl::parse("not a URL").is_err());
        assert!(matches!(
            ResourcePackUrl::parse("ftp://example.invalid/packs/demo.zip"),
            Err(ResourcePackUrlError::UnsupportedScheme(scheme)) if scheme == "ftp"
        ));
        let invalid = ResourcePackUrl::parse("not a URL").expect_err("invalid URL");
        assert_eq!(invalid.to_string(), "relative URL without a base");
        assert!(std::error::Error::source(&invalid).is_none());
        assert!(std::error::Error::source(&ResourcePackUrlError::UnsupportedScheme("ftp".into()))
            .is_none());
    }

    #[test]
    fn ability_mode_changes_preserve_configured_speeds_and_revoke_flight() {
        let mut abilities = Abilities::for_mode(GameMode::Spectator);
        abilities.flying_speed = 0.075;
        abilities.walking_speed = 0.145;
        abilities.set_game_mode(GameMode::Creative);
        assert!(abilities.flying && abilities.may_fly && abilities.instabuild);
        abilities.set_game_mode(GameMode::Adventure);
        assert!(!abilities.flying && !abilities.may_fly && !abilities.may_build);
        abilities.set_game_mode(GameMode::Creative);
        assert!(!abilities.flying && abilities.may_fly && abilities.instabuild);
        assert_eq!(abilities.flying_speed, 0.075);
        assert_eq!(abilities.walking_speed, 0.145);
    }

    /// A protocol whose every method answers with a *distinct* directive, so
    /// "the box forwarded" and "the box fell back to the trait default" are
    /// distinguishable answers rather than both being `None`/empty.
    #[derive(Debug)]
    struct Numbered;

    /// One directive per method, numbered so a mixed-up forward is visible too.
    fn send(id: i32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: id,
            payload: Vec::new(),
        }
    }

    impl ServerProtocol for Numbered {
        fn decode(&self, _state: State, packet_id: i32, _payload: &[u8]) -> ServerBound {
            // Echoes the id back through a variant that carries one, so the
            // arguments are proven to survive the forward, not just the call.
            ServerBound::KeepAlive {
                id: i64::from(packet_id),
            }
        }
        fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
            vec![send(2)]
        }
        fn has_configuration_phase(&self) -> bool {
            false
        }
        fn begin_configuration(&self) -> Vec<ServerDirective> {
            vec![send(3)]
        }
        fn encode_registry_data(&self) -> Vec<ServerDirective> {
            vec![send(4)]
        }
        fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
            vec![send(100 + view_radius)]
        }
        /// Encodes the **spawn** into the answer, not just the view radius, so
        /// "the box forwarded `begin_play_at`" and "the box took the default,
        /// which discards `spawn` and calls `begin_play`" are different values.
        /// Without this override both sides would answer `send(100 + radius)` and
        /// the parity assertion would pass with the forward missing. This test
        /// uses a spawn value distinct from the default for that reason.
        fn begin_play_at(&self, view_radius: i32, spawn: Vec3, mode: GameMode) -> Vec<ServerDirective> {
            let mode = match mode {
                GameMode::Survival => 0,
                GameMode::Creative => 1,
                GameMode::Adventure => 2,
                GameMode::Spectator => 3,
            };
            vec![send(
                300 + view_radius + spawn.x as i32 + spawn.y as i32 + spawn.z as i32 + mode,
            )]
        }
        // Overridden for the same reason `begin_play_at` is: both have
        // emit-nothing defaults, so a missing forward on the box would pass a
        // parity assertion built on the defaults.
        fn encode_game_mode(&self, _mode: GameMode) -> ServerDirective {
            send(401)
        }
        fn encode_player_abilities(&self, _abilities: Abilities) -> ServerDirective {
            send(402)
        }
        fn begin_chunk_batch(&self) -> ServerDirective {
            send(5)
        }
        fn encode_chunk(&self, cx: i32, cz: i32, _column: &ChunkColumn) -> ServerDirective {
            send(cx * 1000 + cz)
        }
        fn try_encode_chunk(
            &self,
            cx: i32,
            cz: i32,
            column: &ChunkColumn,
        ) -> Result<ServerDirective, ChunkEncodeError> {
            if (cx, cz) == (3, 4) {
                return Err(ChunkEncodeError::new("numbered encoder rejected this column"));
            }
            Ok(self.encode_chunk(cx, cz, column))
        }
        fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
            send(200 + batch_size)
        }
        fn welcome_message(&self) -> Vec<ServerDirective> {
            vec![send(8)]
        }
        fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
            send(entity.id)
        }
        fn encode_entity_update(
            &self,
            _prev: Option<&EntitySnapshot>,
            current: &EntitySnapshot,
        ) -> Vec<ServerDirective> {
            vec![send(current.id + 1)]
        }
        fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
            send(ids.len() as i32)
        }
        fn encode_set_entity_data(&self, entity_id: i32, fields: &[MetadataField]) -> ServerDirective {
            send(700 + entity_id * 10 + fields.len() as i32)
        }
        // Overridden for the same reason `encode_set_entity_data` above is: the
        // trait default emits nothing, so a missing forward on the box would pass
        // a parity assertion built on the default alone.
        fn encode_set_entity_link(&self, source_id: i32, target_id: Option<i32>) -> ServerDirective {
            send(750 + source_id * 10 + target_id.unwrap_or(-1))
        }
        fn encode_explode(&self, centre: Vec3, radius: f32) -> ServerDirective {
            send(800 + centre.x as i32 + radius as i32)
        }
        fn encode_keep_alive(&self, id: i64) -> ServerDirective {
            send(id as i32)
        }
        fn encode_set_time(&self, game_time: i64, _day_time: Option<i64>) -> ServerDirective {
            send(game_time as i32)
        }
        fn encode_chunk_cache_center(&self, cx: i32, cz: i32) -> ServerDirective {
            send(cx * 10 + cz)
        }
        fn encode_forget_chunk(&self, cx: i32, cz: i32) -> ServerDirective {
            send(cx * 100 + cz)
        }
        fn encode_block_update(&self, x: i32, y: i32, z: i32, _state: &str) -> ServerDirective {
            send(x + y + z)
        }
        fn encode_air_supply_update(&self, air: i32) -> ServerDirective {
            send(air)
        }
        fn encode_set_experience(&self, progress: f32, level: i32, total: i32) -> ServerDirective {
            let _ = progress;
            send(level * 10_000 + total)
        }
        fn encode_set_health(&self, health: f32, food: i32, saturation: f32) -> ServerDirective {
            let _ = saturation;
            send(health as i32 * 100 + food)
        }
        fn encode_update_attributes(&self, attributes: &[EntityAttributeSnapshot]) -> ServerDirective {
            send(1950 + attributes.len() as i32)
        }
        fn encode_change_difficulty(&self, difficulty: Difficulty, locked: bool) -> ServerDirective {
            send(difficulty as i32 * 10 + i32::from(locked))
        }
        fn encode_game_rule_values(&self, entries: &[(String, String)]) -> ServerDirective {
            send(entries.len() as i32)
        }
        fn encode_game_event(&self, kind: u8, value: f32) -> ServerDirective {
            send(900 + i32::from(kind) * 100 + value as i32)
        }
        fn encode_open_screen(&self, window_id: i32, _menu: &str, _title: &str) -> ServerDirective {
            send(300 + window_id)
        }
        fn encode_container_content(
            &self,
            window_id: i32,
            state_id: i32,
            items: &[Option<ItemStack>],
            _carried: Option<&ItemStack>,
        ) -> ServerDirective {
            send(400 + window_id * 10 + state_id + items.len() as i32)
        }
        fn encode_container_slot(
            &self,
            window_id: i32,
            state_id: i32,
            slot: i32,
            _item: Option<&ItemStack>,
        ) -> ServerDirective {
            send(500 + window_id * 100 + state_id * 10 + slot)
        }
        fn encode_container_data(&self, window_id: i32, property: i32, value: i32) -> ServerDirective {
            send(600 + window_id * 100 + property * 10 + value)
        }
        fn encode_set_held_slot(&self, slot: u8) -> ServerDirective {
            send(650 + i32::from(slot))
        }
        fn encode_initialize_border(&self, border: &crate::border::WorldBorder) -> ServerDirective {
            send(1000 + border.size() as i32)
        }
        fn encode_set_border_center(&self, x: f64, z: f64) -> ServerDirective {
            send(1100 + x as i32 + z as i32)
        }
        fn encode_set_border_lerp_size(
            &self,
            old_size: f64,
            new_size: f64,
            lerp_time_ms: i64,
        ) -> ServerDirective {
            send(1200 + old_size as i32 + new_size as i32 + lerp_time_ms as i32)
        }
        fn encode_set_border_size(&self, size: f64) -> ServerDirective {
            send(1300 + size as i32)
        }
        fn encode_set_border_warning_delay(&self, warning_time: i32) -> ServerDirective {
            send(1400 + warning_time)
        }
        fn encode_set_border_warning_distance(&self, warning_blocks: i32) -> ServerDirective {
            send(1500 + warning_blocks)
        }
        fn encode_resource_pack_push(&self, push: &ResourcePackPush) -> ServerDirective {
            send(
                1600 + push.url.as_str().len() as i32
                    + push.hash.len() as i32
                    + i32::from(push.required),
            )
        }
        fn encode_update_advancements(
            &self,
            update: &crate::advancements::AdvancementUpdate,
        ) -> ServerDirective {
            send(1700 + update.added.len() as i32 + update.removed.len() as i32 + i32::from(update.reset))
        }
        fn encode_award_stats(&self, stats: &[(crate::advancements::StatKey, i32)]) -> ServerDirective {
            send(1800 + stats.len() as i32)
        }
        fn encode_select_advancements_tab(&self, tab: Option<&str>) -> ServerDirective {
            send(1900 + tab.map_or(0, |t| t.len() as i32))
        }
        fn worldgen_scope(&self) -> WorldgenScope {
            // Non-default on purpose: this is the value that proves the box
            // forward works rather than both sides silently using the trait
            // default (the exact failure the `a_boxed_protocol_answers...`
            // control section exists to catch).
            WorldgenScope::V26_2
        }
    }

    fn snapshot(id: i32) -> EntitySnapshot {
        EntitySnapshot {
            id,
            uuid: Uuid::nil(),
            entity_type: ResourceKey::new("minecraft", "pig").expect("static key is valid"),
            position: Vec3::new(0.0, 0.0, 0.0),
            rotation: Rotation { yaw: 0.0, pitch: 0.0 },
            head_yaw: 0.0,
            velocity: Vec3::new(0.0, 0.0, 0.0),
            metadata: Vec::new(),
            object_data: 0,
            leash_link: None,
        }
    }

    /// Every [`ServerProtocol`] method must answer identically through a
    /// `Box<dyn ServerProtocol>` and through the concrete value.
    ///
    /// This is the control for the forwarding impl above, and the reason it is
    /// worth writing is that **most of the methods have defaults**: forgetting
    /// to forward one is not a compile error, it silently answers
    /// `ServerDirective::None`. That failure only ever shows up in a boxed
    /// server — i.e. only in singleplayer, which is exactly the path with no
    /// live oracle to catch it.
    #[test]
    fn a_boxed_protocol_answers_exactly_as_the_concrete_one_does() {
        let direct = Numbered;
        let boxed: Box<dyn ServerProtocol> = Box::new(Numbered);
        let column = ChunkColumn::new(-64, 384);
        let entity = snapshot(77);

        assert_eq!(
            boxed.decode(State::Play, 42, &[]),
            direct.decode(State::Play, 42, &[])
        );
        assert_eq!(
            boxed.login_success("a", Uuid::nil()),
            direct.login_success("a", Uuid::nil())
        );
        assert_eq!(
            boxed.has_configuration_phase(),
            direct.has_configuration_phase()
        );
        assert_eq!(boxed.begin_configuration(), direct.begin_configuration());
        assert_eq!(
            boxed.encode_registry_data(),
            direct.encode_registry_data()
        );
        assert_eq!(boxed.begin_play(7), direct.begin_play(7));
        // This test covers the forwarding contract: `begin_play_at`'s default
        // discards its `spawn` argument, so
        // an unforwarded box answers with the family's hardcoded literal instead.
        // `Numbered` overrides it, so the two sides differ unless the forward
        // exists.
        let spawn = Vec3::new(-101.0, 71.0, 202.0);
        assert_eq!(
            boxed.begin_play_at(7, spawn, GameMode::Creative),
            direct.begin_play_at(7, spawn, GameMode::Creative)
        );
        assert_eq!(
            boxed.encode_game_mode(GameMode::Creative),
            direct.encode_game_mode(GameMode::Creative)
        );
        let abilities = Abilities::for_mode(GameMode::Creative);
        assert_eq!(
            boxed.encode_player_abilities(abilities),
            direct.encode_player_abilities(abilities)
        );
        assert_eq!(boxed.begin_chunk_batch(), direct.begin_chunk_batch());
        assert_eq!(
            boxed.encode_chunk(3, 4, &column),
            direct.encode_chunk(3, 4, &column)
        );
        assert_eq!(boxed.end_chunk_batch(9), direct.end_chunk_batch(9));
        assert_eq!(boxed.welcome_message(), direct.welcome_message());
        assert_eq!(
            boxed.encode_add_entity(&entity),
            direct.encode_add_entity(&entity)
        );
        assert_eq!(
            boxed.encode_entity_update(None, &entity),
            direct.encode_entity_update(None, &entity)
        );
        assert_eq!(
            boxed.encode_remove_entity(&[1, 2, 3]),
            direct.encode_remove_entity(&[1, 2, 3])
        );
        let fields = [MetadataField::CreeperSwellDir(1), MetadataField::CreeperIgnited(true)];
        assert_eq!(
            boxed.encode_set_entity_data(9, &fields),
            direct.encode_set_entity_data(9, &fields)
        );
        assert_eq!(
            boxed.encode_set_entity_link(9, Some(11)),
            direct.encode_set_entity_link(9, Some(11))
        );
        assert_eq!(
            boxed.encode_explode(Vec3::new(1.0, 2.0, 3.0), 3.0),
            direct.encode_explode(Vec3::new(1.0, 2.0, 3.0), 3.0)
        );
        assert_eq!(boxed.encode_keep_alive(11), direct.encode_keep_alive(11));
        assert_eq!(
            boxed.encode_set_time(13, Some(1)),
            direct.encode_set_time(13, Some(1))
        );
        assert_eq!(
            boxed.encode_chunk_cache_center(2, 5),
            direct.encode_chunk_cache_center(2, 5)
        );
        assert_eq!(
            boxed.encode_forget_chunk(2, 5),
            direct.encode_forget_chunk(2, 5)
        );
        assert_eq!(
            boxed.encode_block_update(1, 2, 3, "minecraft:stone"),
            direct.encode_block_update(1, 2, 3, "minecraft:stone")
        );
        assert_eq!(
            boxed.encode_air_supply_update(19),
            direct.encode_air_supply_update(19)
        );
        assert_eq!(
            boxed.encode_set_health(4.0, 20, 5.0),
            direct.encode_set_health(4.0, 20, 5.0)
        );
        let armor_snapshot = [EntityAttributeSnapshot {
            attribute: "minecraft:armor".parse().expect("valid identifier"),
            base: 6.0,
            modifiers: Vec::new(),
        }];
        assert_eq!(
            boxed.encode_update_attributes(&armor_snapshot),
            direct.encode_update_attributes(&armor_snapshot)
        );
        assert_eq!(
            boxed.encode_change_difficulty(Difficulty::Hard, true),
            direct.encode_change_difficulty(Difficulty::Hard, true)
        );
        let rules = [("doDaylightCycle".to_string(), "false".to_string())];
        assert_eq!(
            boxed.encode_game_rule_values(&rules),
            direct.encode_game_rule_values(&rules)
        );
        assert_eq!(
            boxed.encode_open_screen(7, "minecraft:furnace", "Furnace"),
            direct.encode_open_screen(7, "minecraft:furnace", "Furnace")
        );
        assert_eq!(
            boxed.encode_game_event(7, 0.5),
            direct.encode_game_event(7, 0.5)
        );
        let items = [None, Some(ItemStack::new(
            ResourceKey::new("minecraft", "coal").expect("static key is valid"),
            1,
        ))];
        assert_eq!(
            boxed.encode_container_content(7, 1, &items, None),
            direct.encode_container_content(7, 1, &items, None)
        );
        assert_eq!(
            boxed.encode_container_slot(7, 1, 2, items[1].as_ref()),
            direct.encode_container_slot(7, 1, 2, items[1].as_ref())
        );
        assert_eq!(
            boxed.encode_container_data(7, 0, 42),
            direct.encode_container_data(7, 0, 42)
        );
        assert_eq!(
            boxed.encode_set_held_slot(4),
            direct.encode_set_held_slot(4)
        );
        let border = crate::border::WorldBorder::default();
        assert_eq!(
            boxed.encode_initialize_border(&border),
            direct.encode_initialize_border(&border)
        );
        assert_eq!(
            boxed.encode_set_border_center(1.0, 2.0),
            direct.encode_set_border_center(1.0, 2.0)
        );
        assert_eq!(
            boxed.encode_set_border_lerp_size(1000.0, 100.0, 20000),
            direct.encode_set_border_lerp_size(1000.0, 100.0, 20000)
        );
        assert_eq!(
            boxed.encode_set_border_size(512.0),
            direct.encode_set_border_size(512.0)
        );
        assert_eq!(
            boxed.encode_set_border_warning_delay(15),
            direct.encode_set_border_warning_delay(15)
        );
        assert_eq!(
            boxed.encode_set_border_warning_distance(5),
            direct.encode_set_border_warning_distance(5)
        );
        let push = ResourcePackPush {
            id: Uuid::nil(),
            url: "https://example.com/pack.zip"
                .parse()
                .expect("valid resource-pack URL"),
            hash: "0123456789abcdef".to_owned(),
            required: true,
            prompt: None,
        };
        assert_eq!(
            boxed.encode_resource_pack_push(&push),
            direct.encode_resource_pack_push(&push)
        );
        let advancement_update = crate::advancements::AdvancementUpdate {
            reset: true,
            added: vec![crate::advancements::Advancement::new(
                "minecraft:story/root",
                vec![vec!["crafting_table".to_string()]],
                true,
            )],
            removed: vec!["minecraft:story/removed".to_string()],
            progress: Vec::new(),
            show_advancements: true,
        };
        assert_eq!(
            boxed.encode_update_advancements(&advancement_update),
            direct.encode_update_advancements(&advancement_update)
        );
        let stats = [(
            crate::advancements::StatKey::new(crate::advancements::StatType::Mined, "minecraft:stone"),
            3,
        )];
        assert_eq!(
            boxed.encode_award_stats(&stats),
            direct.encode_award_stats(&stats)
        );
        assert_eq!(
            boxed.encode_select_advancements_tab(Some("minecraft:story/root")),
            direct.encode_select_advancements_tab(Some("minecraft:story/root"))
        );
        assert_eq!(
            boxed.encode_select_advancements_tab(None),
            direct.encode_select_advancements_tab(None)
        );
        assert_eq!(
            boxed.worldgen_scope(),
            direct.worldgen_scope(),
            "the box must forward worldgen_scope, not answer with the trait default"
        );

        // -- control ---------------------------------------------------------
        // Every assertion above compares two answers, so it would also pass if
        // *both* sides were the trait default. This is the premise that says
        // they are not: the spy's answers differ from what an unforwarded
        // defaulted method would produce.
        assert_ne!(direct.encode_keep_alive(11), ServerDirective::None);
        assert_ne!(direct.welcome_message(), Vec::<ServerDirective>::new());
        assert_ne!(
            direct.encode_change_difficulty(Difficulty::Hard, true),
            ServerDirective::None
        );
        assert_ne!(direct.encode_game_rule_values(&rules), ServerDirective::None);
        assert_ne!(
            direct.encode_open_screen(7, "minecraft:furnace", "Furnace"),
            ServerDirective::None
        );
        assert_ne!(direct.encode_game_event(7, 0.5), ServerDirective::None);
        assert_ne!(
            direct.encode_container_content(7, 1, &items, None),
            ServerDirective::None
        );
        assert_ne!(
            direct.encode_container_slot(7, 1, 2, items[1].as_ref()),
            ServerDirective::None
        );
        assert_ne!(
            direct.encode_container_data(7, 0, 42),
            ServerDirective::None
        );
        assert_ne!(direct.encode_set_entity_data(9, &fields), ServerDirective::None);
        assert_ne!(
            direct.encode_explode(Vec3::new(1.0, 2.0, 3.0), 3.0),
            ServerDirective::None
        );
        assert_ne!(direct.encode_initialize_border(&border), ServerDirective::None);
        assert_ne!(
            direct.encode_set_border_center(1.0, 2.0),
            ServerDirective::None
        );
        assert_ne!(
            direct.encode_set_border_lerp_size(1000.0, 100.0, 20000),
            ServerDirective::None
        );
        assert_ne!(direct.encode_set_border_size(512.0), ServerDirective::None);
        assert_ne!(
            direct.encode_set_border_warning_delay(15),
            ServerDirective::None
        );
        assert_ne!(
            direct.encode_set_border_warning_distance(5),
            ServerDirective::None
        );
        assert_ne!(direct.encode_resource_pack_push(&push), ServerDirective::None);
        assert_ne!(
            direct.encode_update_advancements(&advancement_update),
            ServerDirective::None
        );
        assert_ne!(direct.encode_award_stats(&stats), ServerDirective::None);
        assert_ne!(
            direct.encode_select_advancements_tab(Some("minecraft:story/root")),
            ServerDirective::None
        );
        assert_ne!(direct.encode_select_advancements_tab(None), ServerDirective::None);
        assert_ne!(
            direct.worldgen_scope(),
            WorldgenScope::None,
            "worldgen_scope answered with the trait default through the box, so the \
             forward is missing and a boxed v770 would silently report 'no worldgen'"
        );
    }

    #[test]
    fn fallible_chunk_encoding_forwards_through_a_box() {
        let direct = Numbered;
        let boxed: Box<dyn ServerProtocol> = Box::new(Numbered);
        let column = ChunkColumn::new(-64, 384);

        assert_eq!(
            direct.try_encode_chunk(5, 6, &column),
            Ok(direct.encode_chunk(5, 6, &column)),
            "the default success path must retain the protocol's exact directive"
        );
        assert_eq!(
            boxed.try_encode_chunk(3, 4, &column),
            direct.try_encode_chunk(3, 4, &column),
            "a boxed protocol must preserve the fallible chunk-encode result"
        );
        assert_eq!(
            boxed.try_encode_chunk(5, 6, &column),
            direct.try_encode_chunk(5, 6, &column),
            "a boxed protocol must preserve successful fallible chunk bytes too"
        );
    }

    #[test]
    fn a_boxed_chunk_encoder_forwards_a_failure() {
        struct RefusingEncoder;

        impl ChunkEncoder for RefusingEncoder {
            fn encode_chunk(&self, _cx: i32, _cz: i32, _column: &ChunkColumn) -> ServerDirective {
                send(91)
            }

            fn try_encode_chunk(
                &self,
                _cx: i32,
                _cz: i32,
                _column: &ChunkColumn,
            ) -> Result<ServerDirective, ChunkEncodeError> {
                Err(ChunkEncodeError::new("encoder failure"))
            }
        }

        let encoder: Box<dyn ChunkEncoder> = Box::new(RefusingEncoder);
        let column = ChunkColumn::new(-64, 384);
        assert_eq!(
            encoder.try_encode_chunk(0, 0, &column),
            Err(ChunkEncodeError::new("encoder failure")),
            "the boxed encoder must not silently call the infallible fallback"
        );
    }

    #[test]
    fn default_checked_encoders_keep_infallible_directives() {
        struct LegacyEncoder;

        impl ChunkEncoder for LegacyEncoder {
            fn encode_chunk(&self, cx: i32, cz: i32, _column: &ChunkColumn) -> ServerDirective {
                send(cx * 10 + cz)
            }
        }

        struct LegacyProtocol;

        impl ServerProtocol for LegacyProtocol {
            fn decode(&self, _state: State, _packet_id: i32, _payload: &[u8]) -> ServerBound {
                unreachable!("this fixture only encodes chunks")
            }

            fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
                unreachable!("this fixture only encodes chunks")
            }

            fn begin_configuration(&self) -> Vec<ServerDirective> {
                unreachable!("this fixture only encodes chunks")
            }

            fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
                unreachable!("this fixture only encodes chunks")
            }

            fn begin_chunk_batch(&self) -> ServerDirective {
                unreachable!("this fixture only encodes chunks")
            }

            fn encode_chunk(&self, cx: i32, cz: i32, _column: &ChunkColumn) -> ServerDirective {
                send(cx * 100 + cz)
            }

            fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
                unreachable!("this fixture only encodes chunks")
            }
        }

        let column = ChunkColumn::new(-64, 384);
        let encoder = LegacyEncoder;
        let protocol = LegacyProtocol;
        assert_eq!(
            encoder.try_encode_chunk(2, 3, &column),
            Ok(encoder.encode_chunk(2, 3, &column)),
            "the encoder default must retain the original directive"
        );
        assert_eq!(
            protocol.try_encode_chunk(2, 3, &column),
            Ok(protocol.encode_chunk(2, 3, &column)),
            "the protocol default must retain the original directive"
        );
    }

    /// The names of every item-level `fn` inside the top-level item whose
    /// declaration line starts with `anchor`.
    ///
    /// Deliberately crude, and deliberately not a Rust lexer — this repo has
    /// already paid for one of those being wrong about lifetimes. Two facts make
    /// line-shape matching sufficient here, and both are properties of this file
    /// rather than of Rust: a top-level item's closing brace is the only `}` that
    /// ever appears alone at column 0, and a direct member of that item is the
    /// only `fn` that ever appears at exactly four spaces of indent. A `}` inside
    /// a doc comment (this file has fenced Java in one) is prefixed by `///`, and
    /// a nested `fn` inside a default body is indented further.
    fn item_level_fn_names(source: &str, anchor: &str) -> Vec<String> {
        let mut lines = source.lines().skip_while(|l| !l.starts_with(anchor));
        assert!(
            lines.next().is_some(),
            "anchor {anchor:?} matched no line — the parser found nothing, which is a \
             failure to run and not a pass"
        );
        lines
            .take_while(|l| *l != "}")
            .filter_map(|l| {
                let rest = l.strip_prefix("    ")?;
                if rest.starts_with(' ') {
                    return None;
                }
                let rest = rest.strip_prefix("pub ").unwrap_or(rest);
                let name = rest.strip_prefix("fn ")?;
                let end = name.find(|c: char| !c.is_ascii_alphanumeric() && c != '_')?;
                Some(name[..end].to_owned())
            })
            .collect()
    }

    /// **Every [`ServerProtocol`] method has a forward in the `Box<P>` impl.**
    ///
    /// The impl's own doc comment has asked for this in prose since it was
    /// written, and prose is not a rule: at the time this guard was added the box
    /// was missing **eleven** forwards, including all three world-effect encoders
    /// — so every singleplayer session emitted no sounds, no level events and no
    /// particles, silently, because an unforwarded defaulted method answers
    /// `ServerDirective::None` and that is indistinguishable from "nothing
    /// happened".
    ///
    /// `a_boxed_protocol_answers_exactly_as_the_concrete_one_does` above cannot
    /// see this class of gap: it compares two things by a hand-written list, so a
    /// *third* thing — a method nobody added to the list — is invisible to it.
    /// This one enumerates instead of listing.
    #[test]
    fn every_server_protocol_method_is_forwarded_by_the_box_impl() {
        let source = include_str!("protocol/codec.rs");
        let trait_fns = item_level_fn_names(source, "pub trait ServerProtocol");
        let box_fns =
            item_level_fn_names(source, "impl<P: ServerProtocol + ?Sized> ServerProtocol for Box<P>");

        // The floor is a measurement, not a guess: the trait had 64 methods when
        // this guard landed. Without it, an anchor that stopped matching would
        // compare two empty sets and report green — the vacuous-precondition
        // species, and the one this whole test exists to rule out.
        assert!(
            trait_fns.len() >= 60,
            "parsed only {} trait methods (64 when this guard landed) — the anchor or \
             the region scan has drifted, so this gate is measuring nothing",
            trait_fns.len()
        );
        assert!(
            box_fns.len() >= 60,
            "parsed only {} forwards in the Box impl — see above",
            box_fns.len()
        );

        // Collected rather than asserted in the loop: an `assert!` inside the
        // iteration reports one missing forward and leaves the rest as arguments,
        // so a neuter would demonstrate a single arm instead of all eleven.
        let missing: Vec<&String> = trait_fns.iter().filter(|f| !box_fns.contains(f)).collect();
        assert!(
            missing.is_empty(),
            "{} ServerProtocol method(s) have no forward in `impl ServerProtocol for Box<P>`: \
             {missing:?}. Each one silently answers the trait's default (usually \
             ServerDirective::None) for every boxed protocol, i.e. for every singleplayer \
             session, while a directly-owned protocol keeps working.",
            missing.len()
        );

        let stray: Vec<&String> = box_fns.iter().filter(|f| !trait_fns.contains(f)).collect();
        assert!(
            stray.is_empty(),
            "the Box impl forwards {stray:?}, which the trait does not declare — either a \
             rename left a stale arm behind or the region scan is reading the wrong item"
        );
    }
}
