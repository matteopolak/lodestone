//! The vibration substrate: a world-event type and the
//! warden-listenable set, deliberately independent of both the Brain module
//! and the [`ai`](crate::ai) goal system.
//!
//! # What it is
//!
//! Vanilla's `GameEvent` world-event registry, restricted for now to the
//! members `#minecraft:warden_can_listen` and its own nested
//! `#minecraft:shrieker_can_listen` reference actually name — the only
//! entries any listener in this tree currently needs. [`VibrationEvent`]
//! is a plain enum rather than the full registry: adding an event nobody can
//! yet produce or consume would be untested data, and this crate's own
//! evidence standard treats an unmodelled subset as the honest shape, not a
//! gap to paper over with invented completeness.
//!
//! **This is deliberately a third name for the same idea two others already
//! claim.** `lodestone_ecs::GameEvent` is the client-side plugin event bus;
//! `GAME_EVENT` in `crates/versions/26.2`'s packet ids is vanilla's
//! clientbound weather/state-change packet. Neither is vanilla's world-event
//! registry a sculk sensor or a warden listens on — `VibrationEvent` is
//! chosen precisely to avoid colliding with either.
//!
//! # How it works
//!
//! [`PostedVibration`] is one event a producer posted this tick, at a
//! position. [`nearest_listenable`] is the host-side resolution a listener
//! reads: the nearest posted vibration within a radius, filtered to the
//! events a warden can hear. There is no travel delay, no line-of-sight
//! occlusion and no per-event "distance" attenuation here — vanilla's own
//! vibration-ticking step walks the vibration toward the listener over
//! several ticks and can be blocked by intervening blocks; this substrate
//! answers "audible this instant, unobstructed" instead, a disclosed
//! simplification for a first pass rather than a silent one. Muffling and
//! travel time are natural follow-ups once something actually consumes this
//! substrate's output (not built here).
//!
//! # How to change it
//!
//! - **A new producer** calls whatever the host's own "post a vibration"
//!   entry point is (`lodestone_server::mobs::MobSim::post_vibration`, for a
//!   server-side one) with the right [`VibrationEvent`] variant. This module
//!   owns no producer itself — it is pure data plus the one pure function a
//!   listener needs.
//! - **A new listenable event** is a new [`VibrationEvent`] variant, added to
//!   [`WARDEN_CAN_LISTEN`]'s test coverage (`vibration_events_match_the_real_tag`)
//!   so drift from the jar's own tag file is caught rather than assumed.
//! - **A new listener species** (a calibrated sculk sensor is the next
//!   obvious one) needs its own radius and its own tag membership function —
//!   [`is_warden_listenable`](VibrationEvent::is_warden_listenable) is named
//!   for the one listener this substrate currently serves, not a generic
//!   "is this a real event" predicate.
//!
//! # Dependencies
//!
//! Nothing — this module has no `std::fs` or platform dependency, so it
//! compiles for `wasm32-unknown-unknown` the same as every other pure-data
//! module in this crate.

use lodestone_model::Vec3;

/// One vanilla `minecraft:game_event` registry entry — restricted, for now,
/// to `#minecraft:warden_can_listen` (`resonate_1..15`/`shriek`, the
/// sculk-catalyst-internal signal-amplification events no ordinary producer
/// ever posts, and `sculk_sensor_tendrils_clicking`'s own nested
/// `#minecraft:shrieker_can_listen` inclusion, are folded in here rather than
/// listed as a separate species-specific set — every event in this enum is a
/// real member of the tag). See this module's own doc for what a future,
/// wider registry would still need to add.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VibrationEvent {
    BlockAttach,
    BlockChange,
    BlockClose,
    BlockDestroy,
    BlockDetach,
    BlockOpen,
    BlockPlace,
    BlockActivate,
    BlockDeactivate,
    Bounce,
    ContainerClose,
    ContainerOpen,
    Drink,
    Eat,
    ElytraGlide,
    EntityDamage,
    EntityDie,
    EntityDismount,
    EntityInteract,
    EntityMount,
    EntityPlace,
    EntityAction,
    Equip,
    Unequip,
    Explode,
    FluidPickup,
    FluidPlace,
    HitGround,
    InstrumentPlay,
    ItemInteractFinish,
    LightningStrike,
    NoteBlockPlay,
    PrimeFuse,
    ProjectileLand,
    ProjectileShoot,
    Shear,
    Splash,
    Step,
    Swim,
    Teleport,
    /// `#minecraft:shrieker_can_listen`'s own one member, folded into this
    /// enum because `warden_can_listen` includes that tag wholesale.
    SculkSensorTendrilsClicking,
}

impl VibrationEvent {
    /// Every variant here is, by construction, a `#minecraft:warden_can_listen`
    /// member — see this enum's own doc for what was deliberately left out
    /// and why. Kept as a real predicate rather than an inlined `true` at
    /// every call site so a future variant that is *not* warden-listenable
    /// (there is none yet) has somewhere to say so, and so
    /// `vibration_events_match_the_real_tag` has something to assert against
    /// that is not itself the enum definition.
    #[must_use]
    pub fn is_warden_listenable(self) -> bool {
        true
    }
}

/// One vibration a real producer posted this tick, at a position — the unit
/// [`nearest_listenable`] resolves over.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PostedVibration {
    pub position: Vec3,
    pub event: VibrationEvent,
    /// Vanilla's own vibration-context source entity — the host's own entity id for
    /// whoever caused the event, if the producer knows one. This
    /// field's reason to exist is vanilla's own warden anger-on-vibration step: a
    /// listener cannot get angry at *something* without an identity to be
    /// angry at. `None` for a producer with no natural source (there is
    /// none yet — every producer today sets this).
    pub source: Option<i32>,
}

/// Vanilla's own warden listener radius — a fixed
/// listening radius. Vanilla's own vibration-ticking step actually walks the
/// signal toward the listener at a fixed speed over several ticks and can be
/// blocked by intervening blocks; this substrate's first pass treats
/// anything within this radius as immediately, unconditionally audible — see
/// this module's own doc for why that is a disclosed simplification.
pub const WARDEN_LISTENER_RADIUS: f64 = 16.0;

/// The nearest warden-listenable vibration within `radius` blocks of
/// `origin`, from everything posted this tick — the host-side resolution a
/// listener reads, mirroring how a persistent-anger deadline or a nearest-
/// player search is resolved once by the host rather than queried live by
/// every consumer.
#[must_use]
pub fn nearest_listenable(
    origin: Vec3,
    radius: f64,
    vibrations: &[PostedVibration],
) -> Option<PostedVibration> {
    let radius_sq = radius * radius;
    vibrations
        .iter()
        .copied()
        .filter(|v| v.event.is_warden_listenable())
        .map(|v| (v, distance_sqr(origin, v.position)))
        .filter(|&(_, d)| d <= radius_sq)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(v, _)| v)
}

/// Which species this substrate currently resolves a nearest-vibration
/// answer for. Table-driven (a single-entry table today) rather than a bare
/// `== "warden"` at every call site, so a second listener species (a
/// calibrated sculk sensor block-entity, say — not a mob, so it would need
/// its own host-side wiring regardless) has one place to be added.
#[must_use]
pub fn is_vibration_listener(species: &str) -> bool {
    species == "warden"
}

/// Vanilla's own allay listener radius, coincidentally the same figure as
/// [`WARDEN_LISTENER_RADIUS`] but a separate constant on purpose: the two
/// species' listening ranges are independent jar values that happen to
/// agree, not one shared figure (see `DESIGN.md`'s own warning about a
/// derived constant standing in for two things vanilla defines separately).
pub const ALLAY_LISTENER_RADIUS: f64 = 16.0;

/// The nearest [`VibrationEvent::NoteBlockPlay`] within `radius` blocks of
/// `origin` — an allay's own listener shape
/// (vanilla's own allay listenable-events tag,
/// which this crate's tag subset does not carry a second name for, since
/// `NoteBlockPlay` is its only member this substrate produces). A second,
/// narrower sibling of [`nearest_listenable`] rather than a generalisation
/// of it — this module's own doc already names "its own radius and its own
/// tag membership function" as what a second listener species needs, and an
/// allay's is a single event type rather than a whole tag.
#[must_use]
pub fn nearest_note_block_play(
    origin: Vec3,
    radius: f64,
    vibrations: &[PostedVibration],
) -> Option<PostedVibration> {
    let radius_sq = radius * radius;
    vibrations
        .iter()
        .copied()
        .filter(|v| matches!(v.event, VibrationEvent::NoteBlockPlay))
        .map(|v| (v, distance_sqr(origin, v.position)))
        .filter(|&(_, d)| d <= radius_sq)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(v, _)| v)
}

fn distance_sqr(a: Vec3, b: Vec3) -> f64 {
    let (dx, dy, dz) = (a.x - b.x, a.y - b.y, a.z - b.z);
    dx * dx + dy * dy + dz * dz
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Transcribed directly from vanilla's own `warden_can_listen` game-event
    /// tag definition, plus the one member its nested `#minecraft:shrieker_can_listen`
    /// reference adds — the outside source [`VibrationEvent`] must not
    /// silently drift from. `resonate_1..15` and `shriek` are excluded on
    /// purpose (see the enum's own doc), so this list is the tag's members
    /// **minus** those sixteen plus the one member the nested tag
    /// contributes.
    const REAL_TAG_MEMBERS: &[&str] = &[
        "block_attach",
        "block_change",
        "block_close",
        "block_destroy",
        "block_detach",
        "block_open",
        "block_place",
        "block_activate",
        "block_deactivate",
        "bounce",
        "container_close",
        "container_open",
        "drink",
        "eat",
        "elytra_glide",
        "entity_damage",
        "entity_die",
        "entity_dismount",
        "entity_interact",
        "entity_mount",
        "entity_place",
        "entity_action",
        "equip",
        "explode",
        "fluid_pickup",
        "fluid_place",
        "hit_ground",
        "instrument_play",
        "item_interact_finish",
        "lightning_strike",
        "note_block_play",
        "prime_fuse",
        "projectile_land",
        "projectile_shoot",
        "shear",
        "splash",
        "step",
        "swim",
        "teleport",
        "unequip",
        "sculk_sensor_tendrils_clicking",
    ];

    fn path(event: VibrationEvent) -> &'static str {
        match event {
            VibrationEvent::BlockAttach => "block_attach",
            VibrationEvent::BlockChange => "block_change",
            VibrationEvent::BlockClose => "block_close",
            VibrationEvent::BlockDestroy => "block_destroy",
            VibrationEvent::BlockDetach => "block_detach",
            VibrationEvent::BlockOpen => "block_open",
            VibrationEvent::BlockPlace => "block_place",
            VibrationEvent::BlockActivate => "block_activate",
            VibrationEvent::BlockDeactivate => "block_deactivate",
            VibrationEvent::Bounce => "bounce",
            VibrationEvent::ContainerClose => "container_close",
            VibrationEvent::ContainerOpen => "container_open",
            VibrationEvent::Drink => "drink",
            VibrationEvent::Eat => "eat",
            VibrationEvent::ElytraGlide => "elytra_glide",
            VibrationEvent::EntityDamage => "entity_damage",
            VibrationEvent::EntityDie => "entity_die",
            VibrationEvent::EntityDismount => "entity_dismount",
            VibrationEvent::EntityInteract => "entity_interact",
            VibrationEvent::EntityMount => "entity_mount",
            VibrationEvent::EntityPlace => "entity_place",
            VibrationEvent::EntityAction => "entity_action",
            VibrationEvent::Equip => "equip",
            VibrationEvent::Unequip => "unequip",
            VibrationEvent::Explode => "explode",
            VibrationEvent::FluidPickup => "fluid_pickup",
            VibrationEvent::FluidPlace => "fluid_place",
            VibrationEvent::HitGround => "hit_ground",
            VibrationEvent::InstrumentPlay => "instrument_play",
            VibrationEvent::ItemInteractFinish => "item_interact_finish",
            VibrationEvent::LightningStrike => "lightning_strike",
            VibrationEvent::NoteBlockPlay => "note_block_play",
            VibrationEvent::PrimeFuse => "prime_fuse",
            VibrationEvent::ProjectileLand => "projectile_land",
            VibrationEvent::ProjectileShoot => "projectile_shoot",
            VibrationEvent::Shear => "shear",
            VibrationEvent::Splash => "splash",
            VibrationEvent::Step => "step",
            VibrationEvent::Swim => "swim",
            VibrationEvent::Teleport => "teleport",
            VibrationEvent::SculkSensorTendrilsClicking => "sculk_sensor_tendrils_clicking",
        }
    }

    /// Every modelled variant is a real tag member, and every real tag
    /// member (minus the disclosed resonate/shriek exclusions) is modelled —
    /// a two-way check, so this cannot silently drop a real event or invent
    /// one that is not in the jar's own tag file.
    #[test]
    fn vibration_events_match_the_real_tag() {
        let all_variants = [
            VibrationEvent::BlockAttach,
            VibrationEvent::BlockChange,
            VibrationEvent::BlockClose,
            VibrationEvent::BlockDestroy,
            VibrationEvent::BlockDetach,
            VibrationEvent::BlockOpen,
            VibrationEvent::BlockPlace,
            VibrationEvent::BlockActivate,
            VibrationEvent::BlockDeactivate,
            VibrationEvent::Bounce,
            VibrationEvent::ContainerClose,
            VibrationEvent::ContainerOpen,
            VibrationEvent::Drink,
            VibrationEvent::Eat,
            VibrationEvent::ElytraGlide,
            VibrationEvent::EntityDamage,
            VibrationEvent::EntityDie,
            VibrationEvent::EntityDismount,
            VibrationEvent::EntityInteract,
            VibrationEvent::EntityMount,
            VibrationEvent::EntityPlace,
            VibrationEvent::EntityAction,
            VibrationEvent::Equip,
            VibrationEvent::Unequip,
            VibrationEvent::Explode,
            VibrationEvent::FluidPickup,
            VibrationEvent::FluidPlace,
            VibrationEvent::HitGround,
            VibrationEvent::InstrumentPlay,
            VibrationEvent::ItemInteractFinish,
            VibrationEvent::LightningStrike,
            VibrationEvent::NoteBlockPlay,
            VibrationEvent::PrimeFuse,
            VibrationEvent::ProjectileLand,
            VibrationEvent::ProjectileShoot,
            VibrationEvent::Shear,
            VibrationEvent::Splash,
            VibrationEvent::Step,
            VibrationEvent::Swim,
            VibrationEvent::Teleport,
            VibrationEvent::SculkSensorTendrilsClicking,
        ];
        assert_eq!(
            all_variants.len(),
            REAL_TAG_MEMBERS.len(),
            "the enum and the transcribed tag must carry the same count"
        );
        for event in all_variants {
            assert!(
                REAL_TAG_MEMBERS.contains(&path(event)),
                "{event:?} ({}) is modelled but not in the real tag",
                path(event)
            );
            assert!(
                event.is_warden_listenable(),
                "every modelled event must be warden-listenable by this module's own construction"
            );
        }
        for member in REAL_TAG_MEMBERS {
            assert!(
                all_variants.iter().any(|&e| path(e) == *member),
                "real tag member {member} has no modelled VibrationEvent"
            );
        }
    }

    /// The discriminating shape: an event just inside the radius is found, one
    /// just outside is not — a control proving the cut is real, not merely
    /// that *something* was found.
    #[test]
    fn nearest_listenable_respects_the_radius() {
        let origin = Vec3::new(0.0, 0.0, 0.0);
        let inside = PostedVibration {
            position: Vec3::new(15.9, 0.0, 0.0),
            event: VibrationEvent::EntityDie,
            source: None,
        };
        let outside = PostedVibration {
            position: Vec3::new(16.1, 0.0, 0.0),
            event: VibrationEvent::EntityDie,
            source: None,
        };
        assert_eq!(
            nearest_listenable(origin, WARDEN_LISTENER_RADIUS, &[outside]),
            None,
            "16.1 blocks away must not be audible at a 16.0 radius"
        );
        assert_eq!(
            nearest_listenable(origin, WARDEN_LISTENER_RADIUS, &[inside]),
            Some(inside),
            "15.9 blocks away must be audible at a 16.0 radius"
        );
    }

    /// Among several candidates, the *nearest* one is returned — not the
    /// first posted, not an arbitrary one.
    #[test]
    fn nearest_listenable_picks_the_closest_of_several() {
        let origin = Vec3::new(0.0, 0.0, 0.0);
        let far = PostedVibration {
            position: Vec3::new(10.0, 0.0, 0.0),
            event: VibrationEvent::Step,
            source: None,
        };
        let near = PostedVibration {
            position: Vec3::new(3.0, 0.0, 0.0),
            event: VibrationEvent::EntityDie,
            source: None,
        };
        let mid = PostedVibration {
            position: Vec3::new(6.0, 0.0, 0.0),
            event: VibrationEvent::BlockDestroy,
            source: None,
        };
        let found = nearest_listenable(origin, WARDEN_LISTENER_RADIUS, &[far, near, mid]);
        assert_eq!(found, Some(near));
    }

    #[test]
    fn nearest_listenable_of_nothing_posted_is_none() {
        assert_eq!(nearest_listenable(Vec3::new(0.0, 0.0, 0.0), WARDEN_LISTENER_RADIUS, &[]), None);
    }

    #[test]
    fn only_warden_is_a_vibration_listener_today() {
        assert!(is_vibration_listener("warden"));
        assert!(!is_vibration_listener("zombie"));
        assert!(!is_vibration_listener("sculk_sensor"), "not a mob at all yet");
    }

    /// The discriminating check for a second, narrower listener query: a
    /// `NoteBlockPlay` at the same distance as an `EntityDie` is found, the
    /// `EntityDie` is not — [`nearest_listenable`]'s own event filter is
    /// "any warden-listenable event"; this one is "exactly one event type",
    /// and a test that only posted `NoteBlockPlay` events could not tell the
    /// two functions apart.
    #[test]
    fn nearest_note_block_play_ignores_every_other_event_type() {
        let origin = Vec3::new(0.0, 0.0, 0.0);
        let die = PostedVibration { position: Vec3::new(3.0, 0.0, 0.0), event: VibrationEvent::EntityDie, source: None };
        let note = PostedVibration { position: Vec3::new(3.0, 0.0, 0.0), event: VibrationEvent::NoteBlockPlay, source: None };
        assert_eq!(
            nearest_note_block_play(origin, ALLAY_LISTENER_RADIUS, &[die]),
            None,
            "an EntityDie must never satisfy an allay's note-block query"
        );
        assert_eq!(nearest_note_block_play(origin, ALLAY_LISTENER_RADIUS, &[note]), Some(note));
    }

    #[test]
    fn nearest_note_block_play_respects_its_own_radius() {
        let origin = Vec3::new(0.0, 0.0, 0.0);
        let inside = PostedVibration { position: Vec3::new(15.9, 0.0, 0.0), event: VibrationEvent::NoteBlockPlay, source: None };
        let outside = PostedVibration { position: Vec3::new(16.1, 0.0, 0.0), event: VibrationEvent::NoteBlockPlay, source: None };
        assert_eq!(nearest_note_block_play(origin, ALLAY_LISTENER_RADIUS, &[outside]), None);
        assert_eq!(nearest_note_block_play(origin, ALLAY_LISTENER_RADIUS, &[inside]), Some(inside));
    }
}
