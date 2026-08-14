//! The dimensions this server hosts, their geometry, and the seam a connection
//! reaches a *second* dimension through.
//!
//! # What it is
//!
//! Three things that used to be one hardcoded assumption:
//!
//! * [`Dimension`] — the identity and geometry of a level (`min_y`, `height`,
//!   `logical_height`, `coordinate_scale`), transcribed from vanilla's
//!   `DimensionTypes` bootstrap rather than from any wire capture.
//! * [`DimensionalSource`] — a transparent [`ChunkSource`] wrapper that carries
//!   the *other* dimensions' sources alongside the primary one, so a connection
//!   already holding a source can reach the Nether without a new parameter on
//!   `serve_play`'s forty-argument signature.
//! * [`teleport_scale`] and [`scaled_destination`] — vanilla's 8:1 coordinate
//!   scaling, in the one direction-agnostic form that cannot be half-implemented.
//!
//! # How it works
//!
//! `crate::integrated` builds one chunk source per dimension, wraps the overworld
//! one in a [`DimensionalSource`] holding the others, and hands *that* to the
//! connection task. `crate::server`'s portal-travel path then asks
//! [`ChunkSource::sibling`](crate::ChunkSource::sibling) for the destination and
//! points its `SourceRef` at it for the rest of the session (or until the player
//! travels back). Every dimension keeps its own `ChunkStore`, its own edit map and
//! its own generator; nothing is mutated in place to "become" another dimension.
//!
//! # How to change it
//!
//! * **Adding the End** is [`Dimension::End`] plus a source in
//!   [`DimensionalSource`] plus an entry in [`Dimension::from_key`]. The travel
//!   path deliberately does *not* generalise to it: an End portal is not a
//!   coordinate-scaled trip (it lands at a fixed obsidian platform), so reusing
//!   the Nether's destination search would put players inside the void.
//! * **`coordinate_scale` is a ratio, never a constant.** `teleport_scale` is
//!   `from / to`, so the overworld→Nether trip divides by 8 and the return trip
//!   multiplies by 8 *through the same expression*. A "divide by 8" written at one
//!   call site and forgotten at the other puts every returning player near the
//!   world origin — and a round trip that starts at `x = 0` cannot tell the two
//!   apart, which is why `tests/nether_portal_round_trip.rs` spawns far from it.
//!
//! ## Gotchas
//!
//! * **`height` is not `logical_height`, and the Nether needs both.** The Nether
//!   is `min_y 0, height 256, logical_height 128`: chunks are framed against 256
//!   (16 sections on the wire), while nothing may be *placed* above 127. Using
//!   `height` where `logical_height` belongs puts a generated portal above the
//!   bedrock roof; using `logical_height` where `height` belongs mis-slices every
//!   served chunk.
//! * **Scaling applies to the entity's position, not to the block it clicked**,
//!   and only to `x`/`z`. `y` is carried and then clamped into the destination's
//!   placeable range — see [`Dimension::clamp_portal_y`].
//!
//! # Dependencies
//!
//! [`crate::ChunkSource`] only. No protocol, no packet id: the mapping from a
//! [`Dimension`] to a `dimension_type` holder id is the *protocol family's*
//! question and lives behind `ServerProtocol::encode_dimension_change`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::chunk::{ChunkColumn, ChunkSource};

/// A level this server can host.
///
/// Deliberately **not** an open-ended registry: every variant here needs a
/// generator, a chunk store, a wire holder id and a travel rule, so a variant with
/// no source behind it would be an island with a plausible name. The End is absent
/// for exactly that reason — its generator exists in `lodestone-worldgen`, and
/// nothing here can yet put a player in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dimension {
    /// `minecraft:overworld`.
    Overworld,
    /// `minecraft:the_nether`.
    Nether,
}

impl Dimension {
    /// Every dimension, in holder order.
    pub const ALL: [Dimension; 2] = [Dimension::Overworld, Dimension::Nether];

    /// The level's resource key, as `login`/`respawn` spell it on the wire and as
    /// `player_data`'s `Dimension` NBT field stores it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Overworld => "minecraft:overworld",
            Self::Nether => "minecraft:the_nether",
        }
    }

    /// Parses a level key. `None` for a dimension this server does not host —
    /// including `minecraft:the_end`, which is *not* an error to see in a saved
    /// player file, only something we cannot put anyone back into.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "minecraft:overworld" => Some(Self::Overworld),
            "minecraft:the_nether" => Some(Self::Nether),
            _ => None,
        }
    }

    /// The lowest world `y` — `DimensionType`'s `minY`.
    #[must_use]
    pub fn min_y(self) -> i32 {
        match self {
            Self::Overworld => -64,
            Self::Nether => 0,
        }
    }

    /// The number of `y` levels a chunk covers — `DimensionType`'s `height`, and
    /// what frames a chunk packet's section count. See the module doc's gotcha.
    #[must_use]
    pub fn height(self) -> i32 {
        match self {
            Self::Overworld => 384,
            Self::Nether => 256,
        }
    }

    /// The highest `y` anything may be placed at, exclusive of the offset —
    /// `DimensionType`'s `logicalHeight`. 128 in the Nether, which is what keeps
    /// a generated portal below the bedrock roof.
    #[must_use]
    pub fn logical_height(self) -> i32 {
        match self {
            Self::Overworld => 384,
            Self::Nether => 128,
        }
    }

    /// The highest world `y` a chunk covers (`min_y + height - 1`).
    #[must_use]
    pub fn max_y(self) -> i32 {
        self.min_y() + self.height() - 1
    }

    /// `DimensionType`'s `coordinateScale` — 1.0 in the overworld, 8.0 in the
    /// Nether. Only ever consumed as a *ratio*; see [`teleport_scale`].
    #[must_use]
    pub fn coordinate_scale(self) -> f64 {
        match self {
            Self::Overworld => 1.0,
            Self::Nether => 8.0,
        }
    }

    /// Whether this dimension has a sky-light source. False in the Nether, which
    /// is what makes its ceiling a ceiling.
    #[must_use]
    pub fn has_skylight(self) -> bool {
        matches!(self, Self::Overworld)
    }

    /// Where a nether portal in this dimension leads — vanilla's
    /// `NetherPortalBlock.getPortalDestination`, whose whole rule is
    /// `currentLevel.dimension() == Level.NETHER ? OVERWORLD : NETHER`. **Not a
    /// general "next dimension"**: it is specifically the nether portal's pairing,
    /// which is why it is a two-cycle and why adding the End must not extend it.
    #[must_use]
    pub fn nether_portal_destination(self) -> Dimension {
        match self {
            Self::Overworld => Self::Nether,
            Self::Nether => Self::Overworld,
        }
    }

    /// The highest `y` `crate::portal`'s destination builder may place a block at
    /// — `min(maxY, minY + logicalHeight - 1)`, vanilla's
    /// `PortalForcer.createPortal`'s `maxPlaceableY`.
    ///
    /// 127 in the Nether and 319 in the overworld.
    #[must_use]
    pub fn max_placeable_y(self) -> i32 {
        self.max_y().min(self.min_y() + self.logical_height() - 1)
    }

    /// Clamps a carried `y` into the band a *fresh* portal may be built in —
    /// `PortalForcer.createPortal`'s `Mth.clamp(origin.getY(), minStartY,
    /// maxStartY)` fallback, where `minStartY = max(minY + 1, 70)` and `maxStartY
    /// = maxPlaceableY - 9`.
    ///
    /// So the Nether's band is `70..=118` and the overworld's is `70..=310`. The
    /// floor of 70 is vanilla's literal and is not derived from sea level; it is
    /// what stops a player who entered a portal at `y = 5` from arriving inside
    /// the Nether's bedrock floor. Returns `None` when the band is empty, which is
    /// vanilla's `Optional.empty()` "unable to create a portal".
    #[must_use]
    pub fn clamp_portal_y(self, y: i32) -> Option<i32> {
        let min_start = (self.min_y() + 1).max(70);
        let max_start = self.max_placeable_y() - 9;
        (max_start >= min_start).then(|| y.clamp(min_start, max_start))
    }
}

/// Vanilla's `DimensionType.getTeleportationScale`: `from / to`.
///
/// **One expression for both directions.** Overworld→Nether is `1.0 / 8.0`, and
/// Nether→overworld is `8.0 / 1.0` — the same code, so an implementation cannot
/// get the return trip wrong while getting the outbound one right.
#[must_use]
pub fn teleport_scale(from: Dimension, to: Dimension) -> f64 {
    from.coordinate_scale() / to.coordinate_scale()
}

/// The approximate arrival point for an entity at `(x, y, z)` travelling `from`
/// one dimension `to` another — vanilla's
/// `NetherPortalBlock.getPortalDestination`'s `approximateExitPos`.
///
/// Horizontal only, then `y` clamped into the destination's placeable band.
/// Returns block coordinates because vanilla does: the scaled position is
/// immediately wrapped in a `BlockPos`, and the fractional part is re-derived at
/// the far end from the exit portal's own rectangle.
#[must_use]
pub fn scaled_destination(
    from: Dimension,
    to: Dimension,
    x: f64,
    y: f64,
    z: f64,
) -> Option<(i32, i32, i32)> {
    let scale = teleport_scale(from, to);
    let bx = (x * scale).floor() as i32;
    let bz = (z * scale).floor() as i32;
    let by = to.clamp_portal_y(y.floor() as i32)?;
    Some((bx, by, bz))
}

/// A [`ChunkSource`] that is transparently one dimension's terrain **and** the
/// door to the others.
///
/// Every trait method forwards to `primary`, so wrapping a source changes no
/// behaviour at all; the one addition is
/// [`ChunkSource::sibling`](crate::ChunkSource::sibling), which hands back another
/// dimension's source.
///
/// # Why the siblings live on the source
///
/// `crate::server`'s connection loop already threads a source everywhere it needs
/// terrain, through a `Copy` `SourceRef`. Its `serve_play` has forty parameters
/// across eleven wrapper call sites (two definitions, native and `wasm32`), so a
/// *new* parameter for the dimension bundle is eleven signature changes in the
/// crate's most contended file — for information the connection can equally well
/// ask the source it is already holding. This keeps the multi-dimension change to
/// the code that actually travels.
///
/// The siblings are held as `Arc<dyn ChunkSource>` rather than a second generic
/// parameter because the Nether's concrete source type differs from the
/// overworld's (`NetherChunkSource` vs `OverworldChunkSource`, each behind its own
/// `ChunkStore`), so no single `S` could name both.
///
/// # The links are one-directional, and that is deliberate
///
/// Only the *primary* dimension's wrapper carries siblings; the Nether's carries an
/// empty map. A player's way **home** is not a sibling lookup — `crate::server`'s
/// connection loop still holds the source it joined with, and returning is putting
/// that back. Making the graph a tree rather than a cycle is what keeps these
/// strong `Arc`s from leaking a whole world (a `ChunkStore` each) every time one is
/// opened, which a mutually-referential pair would.
///
/// # The siblings are built on first use, not at world open
///
/// `factory` is called at most once per dimension, the first time something asks
/// for it — which in practice is the first portal trip of a session. That matters
/// because constructing a Nether generator parses the whole `noise_settings/nether`
/// document tree and builds a structure registry, and **every** integration test in
/// this crate opens a world. Paying for a dimension nobody visits would be a cost
/// on every test in the suite for a feature most of them do not exercise.
pub struct DimensionalSource<S> {
    primary: S,
    which: Dimension,
    /// Memo of everything `factory` has produced, so a second trip reuses the first
    /// trip's `ChunkStore` rather than regenerating the destination from scratch —
    /// which would also lose every block the destination portal wrote.
    siblings: Mutex<HashMap<Dimension, Arc<dyn ChunkSource>>>,
    factory: Option<SiblingFactory>,
    portals: crate::portal::PortalIndex,
}

/// Builds another dimension's terrain on demand. See
/// [`DimensionalSource::with_siblings`].
pub type SiblingFactory =
    Arc<dyn Fn(Dimension) -> Option<Arc<dyn ChunkSource>> + Send + Sync + 'static>;

impl<S> DimensionalSource<S> {
    /// Labels `primary` as `which` with **no** other dimensions reachable — the
    /// single-dimension world every constructor produced before portals existed.
    ///
    /// Not the same as leaving a source unwrapped: this one answers
    /// [`ChunkSource::dimension`] and carries a portal index, so a portal can be lit
    /// and its cells recorded. It simply leads nowhere.
    #[must_use]
    pub fn alone(primary: S, which: Dimension, portals: crate::portal::PortalIndex) -> Self {
        Self {
            primary,
            which,
            siblings: Mutex::new(HashMap::new()),
            factory: None,
            portals,
        }
    }

    /// Wraps `primary` as `which`, with `factory` building the other dimensions on
    /// first request and one shared `portals` index across all of them.
    #[must_use]
    pub fn with_siblings(
        primary: S,
        which: Dimension,
        factory: SiblingFactory,
        portals: crate::portal::PortalIndex,
    ) -> Self {
        Self {
            primary,
            which,
            siblings: Mutex::new(HashMap::new()),
            factory: Some(factory),
            portals,
        }
    }

    /// Which dimension `primary` is.
    #[must_use]
    pub fn dimension(&self) -> Dimension {
        self.which
    }

    /// The wrapped source.
    #[must_use]
    pub fn primary(&self) -> &S {
        &self.primary
    }
}

impl<S: std::fmt::Debug> std::fmt::Debug for DimensionalSource<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DimensionalSource")
            .field("which", &self.which)
            .field("links", &self.factory.is_some())
            .finish_non_exhaustive()
    }
}

impl<S: ChunkSource> ChunkSource for DimensionalSource<S> {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.primary.column(cx, cz)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.primary.block_state(x, y, z)
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        self.primary.set_block(x, y, z, name);
    }

    fn block_entity(&self, x: i32, y: i32, z: i32) -> Option<crate::block_entities::BlockEntity> {
        self.primary.block_entity(x, y, z)
    }

    // Issue #504: forwarded for the same reason every method above is — see
    // `chunk.rs`'s `impl ChunkSource for Arc<S>` doc comment, which names
    // exactly this failure mode ("an unforwarded defaulted method would
    // silently answer the trait's own default … instead of asking the real
    // one"). Missing here specifically meant the block-entity tick's new
    // residency gate was a no-op in production: `with_nether` wraps the real
    // `ChunkStore` in this type before handing it to the tick loop, so
    // `world.is_column_resident(..)` was resolving to this impl block's
    // absent-therefore-default `true` one layer above `ChunkStore`'s real
    // answer, for every world with a Nether sibling wired — which is
    // production, not a corner case.
    fn is_column_resident(&self, cx: i32, cz: i32) -> bool {
        self.primary.is_column_resident(cx, cz)
    }

    fn unload(&self, cx: i32, cz: i32) {
        self.primary.unload(cx, cz);
    }

    fn set_retention_radius(&self, view_radius: i32) {
        self.primary.set_retention_radius(view_radius);
    }

    fn world_registries(&self) -> Option<crate::chunk::WorldRegistries> {
        self.primary.world_registries()
    }

    fn dimension(&self) -> Option<Dimension> {
        Some(self.which)
    }

    /// The whole point of this wrapper. `self.which` maps to `None` — a caller
    /// asking for the dimension it is already in should keep the source it has,
    /// and handing back a second `Arc` to the same store would make a no-op
    /// dimension change look like a real one.
    fn sibling(&self, dimension: Dimension) -> Option<Arc<dyn ChunkSource>> {
        if dimension == self.which {
            return None;
        }
        let factory = self.factory.as_ref()?;
        let mut siblings = self.siblings.lock().expect("dimension sibling lock poisoned");
        if let Some(built) = siblings.get(&dimension) {
            return Some(Arc::clone(built));
        }
        // Built under the lock deliberately. Two connections travelling on the same
        // tick would otherwise each construct a Nether, and the loser's would be
        // dropped — taking with it every block the destination portal it just built
        // had written into it.
        let built = factory(dimension)?;
        siblings.insert(dimension, Arc::clone(&built));
        Some(built)
    }

    fn portal_index(&self) -> Option<&crate::portal::PortalIndex> {
        Some(&self.portals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two arms of the scale, against inputs where they differ — and against
    /// the input where they *do not*, as the control.
    ///
    /// `teleport_scale` is one expression for both directions, so the risk is not
    /// that one arm is missing but that the ratio is inverted. The discriminating
    /// input is any `x != 0`: at `x = 0` multiply-by-8, divide-by-8 and doing
    /// nothing at all are byte-identical, which is exactly why the round-trip gate
    /// spawns far from the origin.
    #[test]
    fn the_scale_is_eight_to_one_and_inverts_on_the_way_back() {
        assert_eq!(
            teleport_scale(Dimension::Overworld, Dimension::Nether),
            0.125
        );
        assert_eq!(teleport_scale(Dimension::Nether, Dimension::Overworld), 8.0);

        // 1720.5 / 8 = 215.0625, whose floor is 215 — a coordinate whose eighth is
        // deliberately *not* an integer, so a truncation bug cannot hide.
        let inbound =
            scaled_destination(Dimension::Overworld, Dimension::Nether, 1720.5, 96.0, -523.25)
                .expect("the Nether's placeable band is not empty");
        assert_eq!(inbound.0, 215, "1720.5 / 8 floors to 215");
        assert_eq!(inbound.2, -66, "-523.25 / 8 = -65.40625, which floors to -66");
        assert_eq!(inbound.1, 96, "a carried y inside 70..=118 is unchanged");

        let outbound =
            scaled_destination(Dimension::Nether, Dimension::Overworld, 215.0, 96.0, -66.0)
                .expect("the overworld's placeable band is not empty");
        assert_eq!(outbound.0, 1720);
        assert_eq!(outbound.2, -528);

        // The control: at the origin every hypothesis agrees, so a gate there
        // measures nothing.
        let at_origin =
            scaled_destination(Dimension::Overworld, Dimension::Nether, 0.0, 96.0, 0.0).unwrap();
        assert_eq!((at_origin.0, at_origin.2), (0, 0));
    }

    /// The y clamp's band comes from the record, and the two dimensions differ —
    /// so a single hardcoded band would fail one of these.
    #[test]
    fn the_portal_y_band_is_seventy_to_logical_height_minus_ten() {
        // Nether: maxPlaceableY = min(255, 0 + 128 - 1) = 127, so 70..=118.
        assert_eq!(Dimension::Nether.max_placeable_y(), 127);
        assert_eq!(Dimension::Nether.clamp_portal_y(5), Some(70));
        assert_eq!(Dimension::Nether.clamp_portal_y(200), Some(118));
        assert_eq!(Dimension::Nether.clamp_portal_y(96), Some(96));

        // Overworld: maxPlaceableY = min(319, -64 + 384 - 1) = 319, so 70..=310.
        assert_eq!(Dimension::Overworld.max_placeable_y(), 319);
        assert_eq!(Dimension::Overworld.clamp_portal_y(200), Some(200));
        assert_eq!(Dimension::Overworld.clamp_portal_y(-40), Some(70));
        assert_eq!(Dimension::Overworld.clamp_portal_y(400), Some(310));
    }

    /// A carried `y` of 200 must not put a player above the Nether's bedrock roof
    /// — the trap the brief names, asserted against the roof's own coordinate
    /// (127) rather than against the clamp's output.
    #[test]
    fn a_carried_y_of_two_hundred_lands_below_the_nether_roof() {
        let landed = Dimension::Nether.clamp_portal_y(200).expect("band is not empty");
        assert!(
            landed < 127,
            "y {landed} is at or above the Nether's bedrock roof at 127"
        );
        // And the frame is four tall above the landing spot, so the whole portal
        // fits: `createPortal` requires `y + 4 <= maxPlaceableY`.
        assert!(landed + 4 <= Dimension::Nether.max_placeable_y());
    }

    /// Issue #504's real production bug, reproduced directly: `is_column_resident`
    /// must reach the wrapped `ChunkStore`'s real answer through this wrapper, not
    /// silently answer the trait's own default (`true`) because this `impl` forgot
    /// to forward it. `crate::integrated`'s `with_nether` wraps every real
    /// `ChunkStore` — overworld and Nether alike — in exactly this type before
    /// handing it to the tick loop, so an unforwarded method here is not a corner
    /// case, it is the only path production ever takes.
    #[test]
    fn is_column_resident_forwards_through_the_dimensional_wrapper() {
        let store = crate::chunk_store::ChunkStore::new(crate::overworld_chunk_source(42));
        let wrapped =
            DimensionalSource::alone(store, Dimension::Overworld, crate::portal::PortalIndex::new());

        assert!(
            !wrapped.is_column_resident(0, 0),
            "an untouched column must report not-resident through the wrapper"
        );
        let _ = wrapped.column(0, 0);
        assert!(
            wrapped.is_column_resident(0, 0),
            "a column just generated through the wrapper must report resident — proving \
             `is_column_resident` reaches the real `ChunkStore` rather than silently answering \
             the trait's own default `true` one layer above it"
        );
        assert!(
            !wrapped.is_column_resident(9, 9),
            "a distinct, untouched column must still report not-resident — the positive check \
             above must not be a constant `true`"
        );
    }
}
