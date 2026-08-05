//! `Sim`'s **collision seam**: what this tick's physics, and this tick's
//! dropped items, are actually resolved against -- seam 11 of the sim.rs
//! decomposition
//! sequence. Seam 1 was the test module, `sim/tests.rs`; 2 placement
//! prediction, `sim/placement.rs`; 3 the interaction/combat cluster,
//! `sim/actions.rs`; 4 the per-tick net-apply fold, `sim/net_apply.rs`; 5 the
//! audio cluster, `sim/audio.rs`; 6 the camera cluster, `sim/camera.rs`; 7
//! chunk/mesh streaming, `sim/meshing.rs`; 8 the `audio` *field* out of the
//! struct into the `AudioEngine` resource -- a field dissolution rather than a
//! file split, but `docs/sim-dissolution.md` numbers it in the same sequence,
//! so these five are 9-13. Seams 9-13 landed together.
//!
//! **`sim/meshing.rs`'s own module doc calls seam 7 "the last of the sim.rs
//! decomposition sequence".** That was true when it was written and is not now.
//! It is left exactly as it stands, because this split is a pure move and
//! editing a neighbour's prose is not part of one -- recorded here instead so a
//! reader who arrives through that file is not misled, and in
//! `docs/sim-dissolution.md`, which carries the authoritative seam list.
//!
//! `tick_collision` and `tick_nearby_entities` are the two `&mut self` calls
//! [`Sim::step`] makes per tick to turn *session facts* -- is there a
//! connection, is there a vanilla atlas, is the diagnostic switch on -- into
//! the owned [`PlayerCollision`] / [`NearbyEntities`] resources a scheduled
//! system can read. `item_collision` is the deliberately *different* answer
//! for dropped items, `live_collision` is the 3x3-column snapshot all of them
//! are built from, and `is_live` is the one discriminant they share.
//!
//! # Named `collide.rs`, not `collision.rs`, on purpose
//!
//! `crate::collision` already exists, and a `mod collision;` inside `sim.rs`
//! would make the bare path `collision::` mean `sim::collision` for every
//! reader of the root -- a shadow that compiles fine and misleads. The two
//! are related but not the same layer: `crate::collision` owns the
//! [`CollisionView`] implementations, this file owns the per-tick decision
//! about which of them to hand the ECS.
//!
//! # What stayed behind, and why
//!
//! [`ChunkWorldCollision`] and [`LiveCollisionSource`] -- the two
//! [`CollisionSource`] adapters -- are still defined in `sim.rs`, together
//! with the three-line `chunk_collision` that constructs the first of them.
//! That is not tidiness: `chunk_collision` returns
//! `Arc<ChunkWorldCollision>`, so widening it to `pub(crate)` while its
//! return type stayed private would put a private type in a more-visible
//! signature and trip `private_interfaces`. Keeping both in the root costs
//! nothing, because a root-private item is visible to every descendant --
//! this file and `sim/tests.rs` alike -- and it widens neither.
//!
//! # What widened
//!
//! Seven methods go private -> `pub(crate)`, each because a *sibling* calls it
//! and privacy only cascades downward: `tick_collision`,
//! `tick_nearby_entities` and `item_collision` from `sim/step.rs`'s tick loop;
//! `live_collision` from `sim/step.rs`, `sim/render_sources.rs` and
//! `sim/camera.rs`; `is_live` from those plus `sim/actions.rs` and
//! `sim/tests.rs`; and the `#[cfg(test)]` `set_fluid_state` from
//! `sim/tests.rs`. [`NEARBY_ENTITY_RADIUS`] stays private -- its only reader is
//! `tick_nearby_entities`, in this file.
//!
//! `use super::*;` for the same reason every earlier seam file uses it: this
//! module is a *descendant* of `sim`, so it already has the same visibility
//! into `Sim`'s private fields, into `sim.rs`'s remaining private helpers and
//! into everything `sim.rs` re-exports that `sim::tests` has always had, with
//! no need to enumerate any of it.

use super::*;

impl Sim {
    /// What this tick's physics collides against.
    ///
    /// The *decision* is the shell's — it needs the session, the atlas and the
    /// diagnostic switch — but the geometry is handed to the ECS as an owned
    /// [`PlayerCollision`] so `player_physics` can be a real scheduled system.
    /// See [`CollisionSource`] for why the borrow could not cross that boundary
    /// directly.
    pub(crate) fn tick_collision(&mut self) -> PlayerCollision {
        // No session and no terrain: there is nothing to stand on and nobody to
        // be.
        if self.net.is_none() && self.chunk_world().is_empty() {
            return PlayerCollision::NoWorld;
        }

        if self.vanilla_atlas.is_some() && self.net.is_some() {
            if !self.collide_against_live_world {
                // The negative control, and the one place Stage 4's single store
                // must *not* be used. See `collide_against_live_world`'s doc: the
                // pre-fix behaviour it reproduces is "collide against terrain we
                // do not have", so it has to name an explicitly empty store.
                // Falling through to `chunk_collision()` would collide against the
                // server's real terrain through the demo classifier — where every
                // non-air vanilla id happens to read as solid — and the control
                // would silently stop failing.
                return PlayerCollision::View(Arc::new(ChunkWorldCollision(ChunkWorld::default())));
            }
            // Live path: collide against the server's terrain. This changes
            // *where blocks come from*, not how collision resolves —
            // `LiveCollision` fills the exact same `CollisionView` hooks
            // `WorldCollision` does, so movement stays bit-exact.
            return match self.live_collision() {
                Some(view) => PlayerCollision::View(Arc::new(LiveCollisionSource(view))),
                // The player's own column has not streamed in yet.
                None => PlayerCollision::Pending,
            };
        }

        PlayerCollision::View(self.chunk_collision())
    }

    /// This tick's entity-push neighbourhood — an owned snapshot handed to the
    /// ECS as [`NearbyEntities`] so [`lodestone_ecs::player::player_physics`]
    /// can stay a plain scheduled system, exactly the pattern
    /// [`Self::tick_collision`] already established for [`PlayerCollision`].
    ///
    /// # Which entities: a jar-dumped census, default-**deny**
    ///
    /// [`VersionData::entity_facts`] answers it, from
    /// `lodestone_data::entity_census` — a table generated from a headless 26.2
    /// server dump of all 158 entity types (`EntityCensusOracle.java`). A
    /// neighbour pushes the player only if vanilla's crowd pass reaches
    /// `player.push(neighbour)`, which needs three things: the type is a
    /// `LivingEntity` (the sole caller of `pushEntities()`, at
    /// `LivingEntity.java:3163`), its `pushEntities()` can still see a player
    /// (`Bat.java:95` empties it; `ArmorStand.java:178` narrows it to ridable
    /// minecarts), and its `doPush(Entity)` still reaches `entity.push(this)`
    /// for one (`Parrot.java:390` skips players outright).
    ///
    /// Note this is *not* the neighbour's `isPushable()`. That gates the
    /// **pushee** — it is the `input` of `EntitySelector.pushableBy` — which is
    /// why `lodestone_physics::push::pair_admitted` takes our own
    /// `self_pushable` and never reads the neighbour's. Keying the census on
    /// `isPushable()` would admit boats and minecarts, which both override it
    /// to `true`.
    ///
    /// An unknown type — and a build with no version family compiled in —
    /// reports `false`. That polarity is the whole point. The denylist this
    /// replaced wrongly admitted seven real 26.2 types: `bamboo_raft` and
    /// `bamboo_chest_raft` (its substring check looked for `boat`, and 1.21.2
    /// named those *rafts*), `splash_potion` and `lingering_potion` (26.2 split
    /// `potion` in two), `ominous_item_spawner`, and the living-but-inert `bat`
    /// and `parrot`. Every one of them would have shoved the player.
    ///
    /// # What the census deliberately excludes
    ///
    /// Boats and rideable minecarts do push players in vanilla, but from their
    /// own ticks — `AbstractBoat.push(Entity)` (`AbstractBoat.java:289`, with a
    /// Y-ordering condition at `:181`) and
    /// `NewMinecartBehavior.pushEntities(AABB)` (`:537`, gated on
    /// `isRideable()` and querying a `1.0E-7`-inflated box). Those cannot join
    /// this list without changing the gate, so the census reports them `false`
    /// rather than approximating them into the wrong pass. See
    /// [`lodestone_model::EntityFacts::pushes_players`].
    pub(crate) fn tick_nearby_entities(&mut self) -> NearbyEntities {
        let center = self.player().position;
        let nearby = self.write(|w| {
            let mut state = w.query::<(&Position, &EntityKind)>();
            // Read once, before the loop. Building the `QueryState` ends the
            // mutable borrow, so the resource handle and the iteration coexist
            // as two immutable reborrows — which is what lets this stay a single
            // `write` pass instead of a resource lookup per candidate.
            let version = w.resource::<VersionData>();
            state
                .iter(w)
                .filter_map(|(pos, kind)| {
                    let feet = Vec3d::new(pos.0.x, pos.0.y, pos.0.z);
                    if (feet.x - center.x).abs() > NEARBY_ENTITY_RADIUS
                        || (feet.y - center.y).abs() > NEARBY_ENTITY_RADIUS
                        || (feet.z - center.z).abs() > NEARBY_ENTITY_RADIUS
                    {
                        return None;
                    }
                    // A type outside the census, or no adapter at all, is a
                    // miss — never a permissive fallthrough.
                    let facts = version.entity_facts(&kind.0)?;
                    if !facts.pushes_players {
                        return None;
                    }
                    // `step_height` plays no part in vanilla's `makeBoundingBox`;
                    // the `RangedAttribute` default is passed so the field never
                    // reads as a real step height resolved from an attribute map.
                    let dims =
                        EntityDimensions::new(facts.dimensions.width, facts.dimensions.height, 0.6);
                    Some(NearbyEntity::living(feet, dims.bounding_box(feet)))
                })
                .collect::<Vec<_>>()
        });
        NearbyEntities(nearby)
    }

    /// What **dropped items** are simulated against this tick.
    ///
    /// Deliberately not [`Self::tick_collision`], and the difference is the whole
    /// reason [`crate::entities::ItemCollision`] is a second resource — see its
    /// docs for the two cases where the player's answer is wrong for an item.
    /// [`Self::live_collision`] is the same 3×3-column snapshot the physics tick
    /// builds; off a live connection there are no tracked items either, so the
    /// offline fallback is never actually asked to resolve real terrain.
    pub(crate) fn item_collision(&self) -> crate::entities::ItemCollision {
        crate::entities::ItemCollision(match self.live_collision() {
            Some(view) => PlayerCollision::View(Arc::new(LiveCollisionSource(view))),
            None => PlayerCollision::View(self.chunk_collision()),
        })
    }

    /// The local player's water/lava submersion this tick, for the shell's
    /// submerged-fog decision (and, later, the underwater overlay, ambient
    /// sounds and swim pose). Version-free and bit-exact — the shell reads this
    /// shared truth rather than deriving its own boolean.
    ///
    /// Written by `lodestone_ecs::player::player_physics` against the very view
    /// movement collided against, so it is consistent with where the tick left
    /// the player.
    #[must_use]
    pub fn fluid_state(&self) -> FluidState {
        self.read(|w| {
            w.get::<Submersion>(self.local)
                .expect("the local player always carries Submersion")
                .0
        })
    }

    /// Overwrite the submersion summary.
    ///
    /// Only for a caller that needs to place the player in a fluid without
    /// simulating one — i.e. a test. Real play never calls this: the value
    /// belongs to the physics producer, and a shell-side write would be exactly
    /// the "invents its own submerged boolean" this type exists to prevent.
    #[cfg(test)]
    pub(crate) fn set_fluid_state(&mut self, fluid: FluidState) {
        self.write_local(|w, local| {
            if let Some(mut submersion) = w.get_mut::<Submersion>(local) {
                submersion.0 = fluid;
            }
        });
    }

    /// Build a [`LiveCollision`] snapshot of the server terrain around the
    /// player, or `None` when the live world can't yet be collided against
    /// (no atlas/net/dimensions, or the player's own column hasn't streamed in).
    ///
    /// Snapshots the 3×3 columns centred on the player over the full vertical
    /// range under a single lock (`sections_at`), returning owned
    /// `Arc<ChunkSection>` handles so no world lock is held while physics queries
    /// it. The 3×3 span covers the player's ±0.3-wide hitbox and its swept path
    /// within a tick; all-air sections are elided by `sections_at` and simply
    /// read as air.
    pub(crate) fn live_collision(&self) -> Option<LiveCollision> {
        let atlas = self.vanilla_atlas.clone()?;
        let net = self.net.as_ref()?;
        let dims = net.world_dimensions()?;
        let min_y = dims.min_y;
        let section_count = dims.section_count();

        let position = self.player().position;
        let pcx = (position.x.floor() as i32).div_euclid(16);
        let pcz = (position.z.floor() as i32).div_euclid(16);

        // Hold the player until the ground under them is known. `sections_at`
        // elides all-air sections to `None`, so an absent section is *not* proof
        // of an unloaded column — key the hold on the column being loaded.
        if !net.is_chunk_loaded(lodestone_client::ChunkPos { x: pcx, z: pcz }) {
            return None;
        }

        let mut requests: Vec<(lodestone_client::ChunkPos, usize)> =
            Vec::with_capacity(9 * section_count);
        for cz in (pcz - 1)..=(pcz + 1) {
            for cx in (pcx - 1)..=(pcx + 1) {
                for si in 0..section_count {
                    requests.push((lodestone_client::ChunkPos { x: cx, z: cz }, si));
                }
            }
        }

        let fetched = net.sections_at(&requests);
        let mut sections = HashMap::new();
        for ((pos, si), section) in requests.iter().zip(fetched) {
            if let Some(section) = section {
                sections.insert((pos.x, pos.z, *si), section);
            }
        }

        Some(LiveCollision::new(
            sections,
            min_y,
            section_count,
            atlas,
            crate::collision::inferred_version_data(),
        ))
    }

    /// Whether this session is rendering a live server world (as opposed to the
    /// offline demo). The stitched vanilla atlas plus a live connection is the
    /// single discriminant used everywhere the live and demo paths diverge.
    pub(crate) fn is_live(&self) -> bool {
        self.vanilla_atlas.is_some() && self.net.is_some()
    }
}

/// Radius, in blocks, within which [`Sim::tick_nearby_entities`] hands a
/// tracked entity to the crowd push as a candidate.
///
/// Vanilla queries `getPushableEntities(this, this.getBoundingBox())` — the
/// *un-inflated* player box — but `docs/entity-push.md`'s own wiring note is
/// explicit that "a generous neighbourhood is fine: candidates that fail a
/// gate contribute nothing". This is a coarse pre-filter, not the gate: the
/// real predicate is `lodestone_physics::push::pair_admitted` downstream, so a
/// too-large radius costs only a few wasted overlap tests while a too-small one
/// **silently drops real candidates** and no test can see it.
///
/// It was `4.0`, chosen for "a happy-ghast-sized neighbour" back when every
/// candidate was handed the player's own `0.6 × 1.8` box. Now that the census
/// supplies real dimensions that value is provably too small, and the bound
/// follows from the census maxima rather than from a guess:
///
/// - widest pusher is `ender_dragon` at `16.0`, and two boxes touch when their
///   centres are within `(0.6 + 16.0) / 2 = 8.3` — so x/z needs `>= 8.3`;
/// - tallest is `giant` at `12.0`, and this compares *feet* to *feet*, so a
///   giant whose feet are `12.0` below ours still overlaps — y needs `>= 12.0`.
///
/// `16.0` is the largest extent in the census and covers both with margin.
/// Deriving it programmatically from the census maxima, rather than restating
/// them here, is the remaining nit — see `docs/entity-push.md`.
const NEARBY_ENTITY_RADIUS: f64 = 16.0;
