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

use lodestone_ecs::entity::EntityUuid;
use lodestone_ecs::{SessionScoreboard, SessionTabList};
use lodestone_physics::CollisionRule;

use super::*;

/// `Team.CollisionRule` (`lodestone_game::scoreboard::CollisionRule`) has no
/// dependency relationship with [`lodestone_physics::push::CollisionRule`] —
/// `lodestone-game` and `lodestone-physics` are siblings, neither depending on
/// the other — so the shell, which depends on both, is the one seam that can
/// convert between the two identical enums.
fn convert_collision_rule(rule: lodestone_game::scoreboard::CollisionRule) -> CollisionRule {
    match rule {
        lodestone_game::scoreboard::CollisionRule::Always => CollisionRule::Always,
        lodestone_game::scoreboard::CollisionRule::Never => CollisionRule::Never,
        lodestone_game::scoreboard::CollisionRule::PushOtherTeams => CollisionRule::PushOtherTeams,
        lodestone_game::scoreboard::CollisionRule::PushOwnTeam => CollisionRule::PushOwnTeam,
    }
}

/// `Entity.getScoreboardName()` — the string a scoreboard team's member list
/// actually carries. `Player.getScoreboardName()` overrides the base
/// `Entity` behaviour to the account name (`Player.java`); every other entity
/// keeps the base `Entity.java` behaviour, its own UUID rendered as a string.
/// Getting this backwards (e.g. keying a player by UUID) would silently miss
/// every real server's `/team join <team> <player-name>`.
fn scoreboard_holder(
    is_player: bool,
    uuid: Option<&EntityUuid>,
    tab_list: Option<&lodestone_game::tablist::TabList>,
) -> Option<String> {
    let uuid = uuid?.0;
    if is_player {
        Some(tab_list?.get(&uuid)?.profile.name.clone())
    } else {
        Some(uuid.to_string())
    }
}

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
    /// `LivingEntity.java`), its `pushEntities()` can still see a player
    /// (`Bat.java` empties it; `ArmorStand.java` narrows it to ridable
    /// minecarts), and its `doPush(Entity)` still reaches `entity.push(this)`
    /// for one (`Parrot.java` skips players outright).
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
    /// own ticks — `AbstractBoat.push(Entity)` (`AbstractBoat.java`, with a
    /// Y-ordering condition at `:181`) and
    /// `NewMinecartBehavior.pushEntities(AABB)` (`:537`, gated on
    /// `isRideable()` and querying a `1.0E-7`-inflated box). Those cannot join
    /// this list without changing the gate, so the census reports them `false`
    /// rather than approximating them into the wrong pass. See
    /// [`lodestone_model::EntityFacts::pushes_players`].
    pub(crate) fn tick_nearby_entities(&mut self) -> NearbyEntities {
        let center = self.player().position;
        let local = self.local;
        let local_uuid = self.local_uuid();
        let (list, self_collision_rule) = self.write(|w| {
            // `w.query()` needs `&mut w`, if only transiently — build the
            // `QueryState` before taking any of the immutable borrows below,
            // exactly as the pre-existing `version` handle already had to
            // (see its own comment): a borrow taken first would have to
            // outlive `query()`'s `&mut`, which the borrow checker refuses.
            let mut state = w.query::<(&Position, &EntityKind, Option<&EntityUuid>)>();

            // Both are on `self.local` in the shell's own mirrored world —
            // the same component `Sim::sidebar` reads `SessionScoreboard`
            // off of — so this is one more immutable reborrow of `w`
            // alongside the query iteration below, not a second world.
            let scoreboard = w
                .get::<SessionScoreboard>(local)
                .map(|board| &board.0);
            let tab_list = w.get::<SessionTabList>(local).map(|list| &list.0);

            // `Entity.getTeam()` for the local player itself — the other half
            // of the team gate that a neighbour's own `CollisionRule` below
            // only supplies one side of. `Player.getScoreboardName()` is the
            // account name, resolved the same way a remote player's is: by
            // uuid through the tab list, not through any component the local
            // player entity carries (it carries none — see
            // `spawn_local_player`).
            let local_name = local_uuid
                .and_then(|uuid| tab_list.and_then(|list| list.get(&uuid)))
                .map(|entry| entry.profile.name.clone());
            let local_team = local_name
                .as_deref()
                .and_then(|name| scoreboard.and_then(|board| board.team_of(name)));
            let self_collision_rule = local_team
                .map(|team| convert_collision_rule(team.collision_rule))
                .unwrap_or_default();
            let local_team_name = local_team.map(|team| team.name.as_str());

            // Read once, before the loop. Building the `QueryState` ends the
            // mutable borrow, so the resource handle and the iteration coexist
            // as two immutable reborrows — which is what lets this stay a single
            // `write` pass instead of a resource lookup per candidate.
            let version = w.resource::<VersionData>();
            let list = state
                .iter(w)
                .filter_map(|(pos, kind, uuid)| {
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
                    let mut neighbour = NearbyEntity::living(feet, dims.bounding_box(feet));

                    // `EntitySelector.pushableBy`'s team gate — see
                    // `lodestone_physics::push::team_allows_push`. A neighbour
                    // outside the scoreboard census (no team, or a team we
                    // failed to resolve a holder key for) keeps
                    // `NearbyEntity::living`'s `Always`/`not allied` default,
                    // which is exactly vanilla's `ownTeam == null` resolution.
                    let is_player = kind.0.path() == "player";
                    if let Some(team) = scoreboard_holder(is_player, uuid, tab_list)
                        .as_deref()
                        .and_then(|holder| scoreboard.and_then(|board| board.team_of(holder)))
                    {
                        neighbour.collision_rule = convert_collision_rule(team.collision_rule);
                        // `Team.isAlliedTo` is reference equality
                        // (`Team.java`) — vanilla has no cross-team alliance,
                        // so "allied" collapses to "same named team", and the
                        // comparison is symmetric regardless of which side
                        // is read as `ownTeam` and which as `theirTeam`.
                        neighbour.allied = local_team_name == Some(team.name.as_str());
                    }
                    Some(neighbour)
                })
                .collect::<Vec<_>>();
            (list, self_collision_rule)
        });
        NearbyEntities {
            list,
            self_collision_rule,
        }
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
    ///
    /// # This is still rebuilt 100–160 times a second, and deliberately so
    ///
    /// Per frame from `update_target` (`sim/step.rs`), again per frame in third
    /// person (`sim/camera.rs`), and twice per tick (`collide.rs`,
    /// `render_sources.rs`). The intermediate `HashMap` this used to fill and
    /// immediately consume is gone (see [`crate::collision::SectionGrid`]), so a
    /// rebuild is now one `sections_at` plus `9 × section_count` `Arc` clones.
    ///
    /// **The remaining win is a memo keyed on `(player chunk, world revision)`, and
    /// it is not buildable today: there is no world-revision signal.**
    /// `lodestone_world::World` is a bare `HashMap<ChunkPos, LoadedChunk>` with no
    /// mutation counter, `lodestone_ecs::ChunkWorld` is an `Arc<RwLock<World>>` that
    /// adds none, and `ClientHandle` exposes none — checked, not assumed. Inventing
    /// one *here* (a hash of the snapshot, a time bound, "the player did not move")
    /// would key the cache on something that is not the thing that changes, and a
    /// stale collision view is a player falling through the world. Left unbuilt on
    /// purpose; the prerequisite is a real revision counter bumped by `World::load`
    /// / `merge` / `unload` and the per-section edit path. `DESIGN.md` §12.114.
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

        // **The loop order is the grid's index order**, x-major over z, so
        // `sections_at`'s aligned response *is* the dense grid
        // `LiveCollision::block_at` indexes — no intermediate `HashMap` to fill
        // and immediately consume. Swapping these two loops transposes the world
        // and nothing here would say so; see `SectionGrid`'s own doc.
        let mut requests: Vec<(lodestone_client::ChunkPos, usize)> =
            Vec::with_capacity(9 * section_count);
        for cx in (pcx - 1)..=(pcx + 1) {
            for cz in (pcz - 1)..=(pcz + 1) {
                for si in 0..section_count {
                    requests.push((lodestone_client::ChunkPos { x: cx, z: cz }, si));
                }
            }
        }

        let sections = crate::collision::SectionGrid::from_aligned(
            net.sections_at(&requests),
            pcx - 1,
            pcz - 1,
            3,
            3,
            section_count,
        );

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

/// Issue #614's last static lead: does `mesher.rs`'s section-to-world-y
/// placement agree with what [`Sim::live_collision`] queries against, in a
/// dimension whose `min_y` differs from the overworld's?
///
/// # Why both sides are expected to agree, and what would make them not
///
/// `TerrainMesh::mesh_column_inner`'s `min_y` comes from
/// `lodestone_ecs::ChunkWorld::extent`; `Sim::live_collision`'s comes from
/// `net.world_dimensions()`, which is `lodestone_client::state::SharedState::world_extent`
/// via `ClientHandle::world_dimensions`. Both are the *same* one-line read —
/// `world.values().next()?.column.min_y()` — over what should be the *same*
/// `Arc<RwLock<lodestone_world::World>>` (`ChunkWorld::from_shared` and
/// `SharedState`'s own store are meant to name one `Arc`; see
/// `lodestone_ecs::chunks`'s module doc for the `is_same_store` authority
/// test). So the two numbers are expected to agree, and this test measures
/// that rather than assuming it: it builds one column via the mesher's own
/// `ChunkWorld`/`WorldExtent` path, places a known block at a known section
/// and local-`y`, computes the mesher's world-`y` for that cell with the real
/// `SectionKey::origin`, and then queries the real
/// `crate::collision::LiveCollision::block_at` — the exact function
/// `Sim::live_collision` builds — at that world-`y`. If the two disagreed on
/// `min_y`, the query would land in the wrong section (or outside the column
/// entirely) and read back air instead of the known block.
///
/// Run once for the overworld's shape (`min_y = -64`, 24 sections) and once
/// for the Nether's (`min_y = 0`, 8 sections) — a single dimension cannot
/// discriminate a mapping that hardcodes the overworld's origin from a
/// correct one, because a wrong constant and a right one both happen to
/// agree when `min_y` is the one they share.
#[cfg(test)]
mod min_y_parity_tests {
    use std::sync::Arc;

    use lodestone_ecs::ChunkWorldWrite;
    use lodestone_world::{
        ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
    };

    use crate::collision::{LiveCollision, SectionGrid};
    use crate::mesher::SectionKey;

    /// A block id that is not air (`0`) and not equal to any section index or
    /// local-`y` used below, so a transposition among (section index,
    /// local-`y`, block id) cannot survive by coincidence.
    const KNOWN_BLOCK: u32 = 77;

    /// The vanilla atlas, or a loud failure naming the fix — `LiveCollision`
    /// takes it as a required constructor parameter (not an inferred
    /// default), so a real one is needed even though this test never reads
    /// occlusion or collision shape through it.
    fn vanilla_atlas() -> Arc<lodestone_render::BlockAtlas> {
        let resources = crate::resources::BlockResources::load(true);
        resources.vanilla_atlas.expect(
            "vanilla assets did not load; set LODESTONE_ASSETS to a pack root with \
             client.jar + generated/reports/blocks.json",
        )
    }

    /// One column of `min_y`/`section_count` shape at chunk `(0, 0)`, with
    /// [`KNOWN_BLOCK`] at local `(5, known_local_y, 9)` inside section
    /// `known_section`. Returns the store's write handle, from which both the
    /// mesher's read handle and a manual section fetch are taken.
    fn shaped_world(
        min_y: i32,
        section_count: usize,
        known_section: usize,
        known_local_y: i32,
    ) -> ChunkWorldWrite {
        let mut column = ChunkColumn::new(
            min_y,
            section_count,
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            0, // air id
            0, // biome id
        );
        let world_y = min_y + (known_section as i32) * 16 + known_local_y;
        column.set_block(5, world_y, 9, KNOWN_BLOCK);

        let loaded = LoadedChunk::new(
            column,
            ColumnLight::new(section_count),
            Heightmaps::new(),
            Vec::new(),
        );
        let mut world = World::new();
        world.load(ChunkPos::new(0, 0), loaded);
        ChunkWorldWrite::new(world)
    }

    /// One dimension's worth of the comparison. `collision_min_y` is passed
    /// separately from the mesher's own reading so a deliberately-wrong value
    /// can be fed through the same check as a control (see the test below).
    ///
    /// Returns `Some(reason)` on a mismatch, so the caller can collect every
    /// failing case into one assertion rather than aborting on the first.
    fn check(
        name: &str,
        min_y: i32,
        section_count: usize,
        collision_min_y: i32,
        atlas: &Arc<lodestone_render::BlockAtlas>,
    ) -> Option<String> {
        let known_section = section_count / 2;
        let known_local_y = 7;
        let write = shaped_world(min_y, section_count, known_section, known_local_y);
        let store = write.read_handle();

        // --- Mesher path: the real `SectionKey::origin`, fed the real
        // `ChunkWorld::extent` this store reports. ---
        let extent = store.extent().expect("one column loaded");
        assert_eq!(extent.min_y, min_y, "{name}: fixture sanity — min_y");
        assert_eq!(
            extent.section_count, section_count,
            "{name}: fixture sanity — section_count"
        );
        let key = SectionKey {
            cx: 0,
            cz: 0,
            si: known_section,
            min_y: extent.min_y,
        };
        let mesh_world_y = key.origin()[1] + known_local_y;

        // --- Collision path: the real `LiveCollision::block_at`, fed
        // `collision_min_y` — the number `Sim::live_collision` would have
        // read from `net.world_dimensions()` for this same store. ---
        let section = write.read().section(ChunkPos::new(0, 0), known_section);
        let mut cells = vec![None; section_count];
        cells[known_section] = section;
        let grid = SectionGrid::from_aligned(cells, 0, 0, 1, 1, section_count);
        let collision = LiveCollision::new(
            grid,
            collision_min_y,
            section_count,
            Arc::clone(atlas),
            None, // version data is irrelevant to block_at's raw state-id read
        );

        let found = collision.block_at(5, mesh_world_y, 9);
        if found == KNOWN_BLOCK {
            None
        } else {
            Some(format!(
                "{name}: mesher placed the known block at world-y {mesh_world_y} \
                 (min_y {min_y}, section {known_section}, local-y {known_local_y}); \
                 collision queried at that same world-y with min_y {collision_min_y} \
                 and read back {found}, not {KNOWN_BLOCK}"
            ))
        }
    }

    /// **The measurement.** For both the overworld's shape and the Nether's —
    /// deliberately different `min_y`/height so a mapping that hardcoded the
    /// overworld's origin could not coincidentally pass — mesher and
    /// collision are fed the *same* `min_y` (as they would be in production,
    /// both reading `ChunkWorld::extent`/`world_extent` off the one store)
    /// and must agree on where a known block sits.
    #[test]
    // `vanilla_atlas()` is a hard `expect`, deliberately — a silent skip here
    // would be the precondition species of vacuous test. But that makes the
    // gate unrunnable wherever `.cache/mc/<version>/` is absent, which is
    // every CI runner, so it belongs with the rest of the jar-dependent
    // corpus rather than failing the workspace suite. Run it with
    // `--ignored` after `cargo xtask fetch-assets`.
    #[ignore = "requires a fetched vanilla client.jar + blocks.json under .cache/mc/<version>/"]
    fn mesher_and_collision_place_the_same_block_at_the_same_world_y_in_both_dimensions() {
        let atlas = vanilla_atlas();
        let mut mismatches = Vec::new();

        // Overworld: min_y = -64, 384 blocks tall = 24 sections.
        if let Some(reason) = check("overworld", -64, 24, -64, &atlas) {
            mismatches.push(reason);
        }
        // The Nether: min_y = 0, 128 blocks tall = 8 sections. Its min_y
        // differs from the overworld's, which is the whole point: a mapping
        // that silently reused -64 here would have nothing else to catch it.
        if let Some(reason) = check("the_nether", 0, 8, 0, &atlas) {
            mismatches.push(reason);
        }

        assert!(
            mismatches.is_empty(),
            "mesher and collision disagree on section-to-world-y placement:\n{}",
            mismatches.join("\n")
        );
    }

    /// **Control.** The same check as above, but the collision side is fed a
    /// deliberately wrong `min_y` (the overworld's `-64`, for a Nether-shaped
    /// world) — proving the detector actually fires on a real mismatch rather
    /// than passing vacuously. Per `DESIGN.md` §12, an assertion of agreement
    /// is only as good as the evidence that the mechanism *would* have caught
    /// disagreement.
    #[test]
    // Ignored for the same reason as the measurement it controls, and it must
    // stay paired with it: a control that runs where its subject does not is
    // proving nothing about that subject.
    #[ignore = "requires a fetched vanilla client.jar + blocks.json under .cache/mc/<version>/"]
    fn the_check_fails_when_collision_is_fed_the_wrong_min_y() {
        let atlas = vanilla_atlas();
        let reason = check("the_nether_with_overworld_min_y", 0, 8, -64, &atlas);
        assert!(
            reason.is_some(),
            "feeding collision the overworld's min_y (-64) for a Nether-shaped \
             world (min_y 0) must be caught as a mismatch — if it is not, the \
             positive result above proves nothing"
        );
    }
}
