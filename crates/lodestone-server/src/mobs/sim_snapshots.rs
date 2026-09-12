//! Entity snapshot projection for every live mob-simulation entity.

use super::*;

impl<'w> MobSim<'w> {
    /// Every live entity this sim owns — mobs, projectiles, dropped items —
    /// lowered to the wire-facing [`EntitySnapshot`] the encode seam needs.
    ///
    /// This is the merged sibling of iterating [`iter`](Self::iter) alone:
    /// [`crate::tick::run_tick_loop`] (previously [`run_mob_tick_loop`])
    /// publishes this (not just the mobs) to [`LiveMobSource`], which is what
    /// actually gets a spawned projectile or
    /// dropped item onto the same `add_entity`/`move_entity`/`remove_entity`
    /// wire path mobs already proved reaches a real client
    /// (`entity_streaming_live.rs`) — without this, ticking the registries
    /// above would still be a closed loop that reaches zero pixels.
    #[must_use]
    pub fn snapshots(&self) -> Vec<EntitySnapshot> {
        let mut out: Vec<EntitySnapshot> = self
            .mobs
            .iter()
            .map(|m| {
                let mut snap = m.snapshot();
                // Only `MobSim` can resolve a `LeashHolder` to a wire entity id
                // (a player holder needs `self.players`) — see
                // `resolve_leash_target`'s own doc for the three shapes.
                snap.leash_link = m.leash_holder().and_then(|holder| self.resolve_leash_target(holder));
                snap
            })
            .collect();
        for t in self.projectiles.iter() {
            if let Some(meta) = self.projectile_meta.get(&t.id) {
                out.push(EntitySnapshot {
                    id: t.id,
                    uuid: meta.uuid,
                    entity_type: meta.entity_type.clone(),
                    position: t.projectile.position,
                    rotation: Rotation::new(0.0, 0.0),
                    head_yaw: 0.0,
                    velocity: t.projectile.velocity,
                    metadata: Vec::new(),
                    // The base arrow entity and friends leave the add-entity
                    // packet's
                    // data at `0`; only add-entity-packet overrides carry one,
                    // and no projectile this sim spawns has one.
                    object_data: 0,
                    // A projectile is never leashable, vanilla's own interface for that.
                    leash_link: None,
                });
            }
        }
        let mut item_ids: Vec<i32> = self.item_state.keys().copied().collect();
        item_ids.sort_unstable();
        for id in item_ids {
            let state = self
                .item_state
                .get(&id)
                .expect("sorted item ids came from the live item state map");
            out.push(EntitySnapshot {
                id,
                // **`minecraft:item`, not the item's own key.** This field is an
                // *entity* type. The fixed item entity type keeps a dropped
                // stack on the item-entity rendering path rather than treating
                // its registry key as an entity type.
                //
                // The item's *identity* belongs in `metadata` instead, at the
                // item-stack metadata slot described below.
                uuid: state.uuid,
                entity_type: item_entity_type(),
                position: state.motion.position,
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: state.motion.velocity,
                // **The field that makes a drop draw at all.** A
                // client draws nothing for an item entity whose stack it has
                // not been told: the renderer returns early on an empty stack,
                // and this project's own
                // client receives the same stack update.
                // The metadata must therefore carry the stack for a block drop
                // to draw while it falls, merges, and remains pickable.
                //
                // This is the **only** place in the tree that constructs a
                // `MetadataField::Item`, and that is load-bearing rather than
                // incidental: the item-stack metadata field's wire index (8) is
                // shared with other entity fields, so the encoder
                // in `crates/protocol/v770/src/server_protocol.rs` relies on
                // every `Item` field belonging to a `minecraft:item` entity by
                // construction. This loop iterates `item_state`, so it does.
                // Never push one from the mob or projectile loops above.
                //
                // The count is the *entity's* stack size and lives on the
                // lifecycle, not on `ItemState` — the same
                // `map_or(1, |l| l.count)` read `merge_neighbouring_items` uses
                // above, with the same default for the (unreachable in
                // practice) case of state without a lifecycle.
                metadata: vec![MetadataField::Item {
                    item: state.item.clone(),
                    count: self.items.get(id).map_or(1, |lifecycle| lifecycle.count),
                }],
                // The stack travels as metadata (above), not as object data.
                object_data: 0,
                // A dropped item is never leashable.
                leash_link: None,
            });
        }
        // `ExperienceOrb`. Iterated in **sorted** id order, like the falling blocks
        // below and unlike the two loops above: an orb's whole visible behaviour is a
        // multi-tick drift toward the player, so a `HashMap` order would reshuffle
        // which of two orbs the snapshot stream updates first every tick.
        let mut orb_ids: Vec<i32> = self.orbs.keys().copied().collect();
        orb_ids.sort_unstable();
        for id in orb_ids {
            let Some(orb) = self.orbs.get(&id) else {
                continue;
            };
            out.push(EntitySnapshot {
                id,
                uuid: orb.uuid,
                entity_type: orb_entity_type(),
                position: orb.motion.position,
                // A random rotation has no consumer: the client billboards the
                // sprite at the camera.
                // Sending a rotation would be sending a value with no consumer.
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: orb.motion.velocity,
                // **The field that decides which of the eleven sprite frames draws.**
                // Vanilla's own icon-bucketing getter buckets the orb's own value getter — not `count`, and not
                // linearly — so an orb whose value never reaches the client draws frame
                // 0 (the smallest) whatever it is worth. Vanilla's own metadata
                // registration registers
                // its own value field and nothing else, so metadata is the only channel;
                // there is no object data on the add-entity packet to carry it.
                //
                // `count` is deliberately *not* sent: vanilla does not synchronise it,
                // and a client that knew it would still draw one sprite.
                metadata: vec![MetadataField::ExperienceOrbValue { value: orb.value }],
                object_data: 0,
                // An experience orb is never leashable, vanilla's own interface for that.
                leash_link: None,
            });
        }
        // The falling-block entity. The **only** producer of a non-zero
        // `object_data` in this crate: vanilla's own add-entity packet passes
        // the block-state id, and that field is the sole channel
        // by which a client learns which block is falling (see
        // `TrackedFallingBlock`'s own doc for why metadata cannot carry it).
        //
        // Iterated in **sorted** id order, unlike the two loops above. A falling
        // block's whole point is a smooth multi-tick animation, and
        // `EntityStreamer::sync` walks this list in order to emit spawns and
        // updates; a `HashMap` order would reshuffle which of two simultaneous
        // falls is updated first from tick to tick for no reason.
        let mut falling_ids: Vec<i32> = self.falling_blocks.keys().copied().collect();
        falling_ids.sort_unstable();
        for id in falling_ids {
            let Some(tracked) = self.falling_blocks.get(&id) else {
                continue;
            };
            out.push(EntitySnapshot {
                id,
                uuid: tracked.uuid,
                entity_type: falling_block_entity_type(),
                position: tracked.motion.position,
                // Vanilla's own falling-block entity never rotates: its own
                // fall step sets no `yRot`/`xRot`
                // and nothing writes them afterwards. A falling block that visibly
                // spun would be a *more* interesting animation and a wrong one.
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: Vec3::new(0.0, tracked.motion.velocity_y, 0.0),
                // Vanilla's own metadata registration registers its own
                // start-position field alone, and that
                // accessor's value is the entity's own spawn cell — which the
                // client recovers from the add-entity packet's position in
                // its own client-side reconstruction. So there is genuinely
                // nothing to send.
                metadata: Vec::new(),
                // `unwrap_or(0)` rather than skipping the entity: an unresolvable
                // state is a data-table gap, and streaming the entity with a wrong
                // texture is a visible bug a reader can chase, while silently
                // dropping it reproduces the original teleport with no trace. The
                // three states `crate::gravity_tick::is_gravity_block` accepts all
                // resolve.
                object_data: block_states::state_id(&tracked.state).unwrap_or(0) as i32,
                // A falling block is never leashable, vanilla's own interface for that.
                leash_link: None,
            });
        }
        // Vehicles — the boats. Sorted ids, like the two loops above and for the
        // same reason: a boat's whole point is a smooth multi-tick glide, so a
        // `HashMap` order would reshuffle which of two boats `EntityStreamer::sync`
        // updates first every tick.
        let mut vehicle_ids: Vec<i32> = self.vehicles.keys().copied().collect();
        vehicle_ids.sort_unstable();
        for id in vehicle_ids {
            let Some(vehicle) = self.vehicles.get(&id) else {
                continue;
            };
            out.push(EntitySnapshot {
                id,
                uuid: vehicle.uuid,
                entity_type: vehicle.entity_type.clone(),
                position: Vec3::new(
                    vehicle.motion.position.x,
                    vehicle.motion.position.y,
                    vehicle.motion.position.z,
                ),
                // **The yaw is the point.** A boat's hull is the only thing that
                // shows which way it faces, and the placing player's action
                // supplies it — a boat streamed at yaw 0 always points south
                // however you placed it. The pitch stays 0.
                rotation: Rotation::new(vehicle.yaw, 0.0),
                // A boat is not a living entity, so there is no separate
                // head rotation to send; the rotate-head packet is only sent
                // for entities that have one.
                head_yaw: 0.0,
                velocity: Vec3::new(
                    vehicle.motion.velocity.x,
                    vehicle.motion.velocity.y,
                    vehicle.motion.velocity.z,
                ),
                // Boat metadata contains paddle-left and paddle-right values,
                // on top of the shared vehicle hurt state.
                //
                // The paddle pair is emitted — the `PADDLE_BOAT`
                // remainder — via `MetadataField::BoatPaddles`, whose own doc
                // has the index-11/12 collision this loop is the guard for
                // (every entry here is a boat by construction, never the
                // living entity/thrown-trident that also claim those
                // indices). Always included, even at its `false, false`
                // default — the same "always included" convention
                // `CreeperSwellDir`'s own doc states, and load-bearing here:
                // a stop-paddling transition must reach a diffing consumer as
                // a real `false, false` rather than as an absent field.
                // Bubble-time metadata stays unsent: nothing in this crate's
                // boat physics tracks a bubble-column timer.
                metadata: vec![
                    crate::protocol::MetadataField::BoatPaddles {
                        left: vehicle.paddle_left,
                        right: vehicle.paddle_right,
                    },
                    // Shared vehicle hurt state. Always included, at its
                    // resting `(0, 1, 0.0)` as well, for `BoatPaddles`' own
                    // stated reason: the *end* of a rock has to reach a diffing
                    // consumer as a real zero rather than as an absent field, or
                    // the hull stays tipped over for as long as the boat exists.
                    crate::protocol::MetadataField::VehicleHurt {
                        time: vehicle.hurt_time,
                        dir: vehicle.hurt_dir,
                        damage: vehicle.damage,
                    },
                ],
                // The boat supplies no additional spawn data.
                object_data: 0,
                // A boat is never leashable.
                leash_link: None,
            });
        }
        // Primed TNT. Sorted ids, for the same reason every other sidecar loop
        // in this method is: a stable per-tick update order for
        // the snapshot stream.
        let mut tnt_ids: Vec<i32> = self.tnt.keys().copied().collect();
        tnt_ids.sort_unstable();
        for id in tnt_ids {
            let Some(t) = self.tnt.get(&id) else {
                continue;
            };
            out.push(EntitySnapshot {
                id,
                uuid: t.uuid,
                entity_type: tnt::tnt_entity_type(),
                position: Vec3::new(t.motion.position.x, t.motion.position.y, t.motion.position.z),
                // A primed TNT entity never rotates — its base
                // entity's rotation fields
                // stay `0.0` for the whole of its short life.
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: Vec3::new(t.motion.velocity.x, t.motion.velocity.y, t.motion.velocity.z),
                // The fuse metadata field — see `MetadataField::TntFuse`'s own
                // doc for why this is index 8's fifth `INT` claimant and must be
                // class-guarded on decode.
                metadata: vec![MetadataField::TntFuse(t.fuse)],
                // No additional spawn data is needed.
                object_data: 0,
                // Never leashable.
                leash_link: None,
            });
        }
        // Minecarts. Sorted ids, for the same reason every other sidecar loop
        // in this method is.
        let mut minecart_ids: Vec<i32> = self.minecarts.keys().copied().collect();
        minecart_ids.sort_unstable();
        for id in minecart_ids {
            let Some(cart) = self.minecarts.get(&id) else {
                continue;
            };
            // Furnace-minecart fuel metadata uses index 13, shared with
            // its own command-block-minecart command-name field (a `STRING`) under a
            // different serializer; this is the only producer of a
            // `MinecartFuel` field and it only ever fires from the furnace
            // loop, so the two can never collide the way `MetadataField::Item`'s
            // own doc describes for index 8.
            let metadata = if cart.kind.is_furnace() {
                vec![MetadataField::MinecartFuel(cart.fuel > 0)]
            } else {
                Vec::new()
            };
            out.push(EntitySnapshot {
                id,
                uuid: cart.uuid,
                entity_type: cart.kind.entity_type(),
                position: Vec3::new(cart.motion.position.x, cart.motion.position.y, cart.motion.position.z),
                rotation: Rotation::new(cart.yaw, 0.0),
                // A minecart is not a living entity; no separate head
                // rotation packet is ever sent for one.
                head_yaw: 0.0,
                velocity: Vec3::new(cart.motion.velocity.x, cart.motion.velocity.y, cart.motion.velocity.z),
                metadata,
                // No additional spawn data is needed.
                object_data: 0,
                // Never leashable.
                leash_link: None,
            });
        }
        // The lightning-bolt entity. Sorted ids for the same
        // reason the two
        // loops above are: a bolt is short-lived but real entities, and a
        // `HashMap` order would reshuffle which of two simultaneous strikes
        // the snapshot stream updates first.
        //
        // **Empty metadata is correct, not an omission**: the lightning entity
        // has no metadata fields,
        // so there is nothing to send.
        let mut bolt_ids: Vec<i32> = self.lightning_bolts.keys().copied().collect();
        bolt_ids.sort_unstable();
        for id in bolt_ids {
            let Some(bolt) = self.lightning_bolts.get(&id) else {
                continue;
            };
            out.push(EntitySnapshot {
                id,
                uuid: bolt.uuid,
                entity_type: lightning::lightning_bolt_entity_type(),
                position: bolt.pos,
                // A bolt never rotates or moves once struck.
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: Vec3::new(0.0, 0.0, 0.0),
                metadata: Vec::new(),
                // No additional spawn data is needed for this lightning entity.
                object_data: 0,
                // Never a `Leashable`.
                leash_link: None,
            });
        }
        self.push_dragon_snapshots(&mut out);
        self.push_end_crystal_snapshots(&mut out);
        self.push_wither_snapshots(&mut out);
        // Live fishing bobbers.
        self.fishing_bobber_snapshots(&mut out);
        // Live raiders spawned by an active raid stream through
        // the ordinary mob loop at the top of this function — `raid.rs`
        // spawns them with `spawn_species`, exactly as a patrol does — so
        // there is nothing to append here.
        out
    }
}
