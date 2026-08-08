//! Section-level visibility: the thing frustum culling alone cannot do.
//!
//! Standing on the surface, the frustum still contains the entire column of
//! sections below you, so frustum culling alone would submit the whole
//! underground. Vanilla avoids this with a two-step scheme, which we implement
//! here:
//!
//! 1. **Per-section connectivity** ([`compute_visibility`]): flood-fill the
//!    non-opaque cells of a section and record, for each of the 15 unordered
//!    face pairs, whether you can pass from one face to the other through open
//!    space. A solid section connects no faces; an empty section connects all.
//!
//! 2. **Graph walk** ([`walk_visible`]): breadth-first from the camera's
//!    section, only stepping into a neighbour if the current section connects
//!    the face we *entered* through to the face we're *leaving* through, and
//!    never reversing direction along an axis. That "can light travel straight
//!    through this section from where I came in to where I'm going" test is what
//!    stops the underground from being drawn.
//!
//! Both steps are pure and unit-tested with no GPU. Frustum culling composes on
//! top via the `in_frustum` predicate passed to [`walk_visible`].

use std::collections::{HashMap, HashSet, VecDeque};

use crate::section::{Face, SECTION_SIZE, SectionView};

/// A section's grid coordinate (in section units, not blocks).
pub type SectionCoord = (i32, i32, i32);

/// Which of the six faces of a section are mutually connected through open
/// space. Symmetric; a face is trivially connected to itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionVisibility {
    connected: [[bool; 6]; 6],
}

impl SectionVisibility {
    /// A section that connects no faces (fully solid / opaque).
    pub const NONE: SectionVisibility = SectionVisibility {
        connected: [[false; 6]; 6],
    };

    /// A fully solid section: no face-to-face connections, but each face is
    /// trivially connected to itself — matching what the flood produces for a
    /// section with no open cells. (Distinct from [`NONE`](Self::NONE), which is
    /// the all-false blocker used directly in graph walks.)
    #[must_use]
    pub fn solid() -> Self {
        let mut connected = [[false; 6]; 6];
        for (i, row) in connected.iter_mut().enumerate() {
            row[i] = true;
        }
        SectionVisibility { connected }
    }

    /// A section that connects every face to every other (fully open).
    #[must_use]
    pub fn all() -> Self {
        SectionVisibility {
            connected: [[true; 6]; 6],
        }
    }

    /// Whether faces `a` and `b` are connected through open space.
    #[must_use]
    pub fn connects(&self, a: Face, b: Face) -> bool {
        self.connected[a.index()][b.index()]
    }

    /// Build a connectivity set from an explicit list of connected face pairs
    /// (symmetric; every face is connected to itself regardless).
    ///
    /// The point of a hand-written constructor is walk-fidelity fixtures: a
    /// section that connects exactly one axis is trivial to state here and
    /// awkward to produce by voxelising a corridor and hoping
    /// [`compute_visibility`]'s sparse shortcut does not simply return
    /// [`all`](Self::all).
    #[must_use]
    pub fn from_pairs(pairs: &[(Face, Face)]) -> Self {
        let mut connected = [[false; 6]; 6];
        for (i, row) in connected.iter_mut().enumerate() {
            row[i] = true;
        }
        for (a, b) in pairs {
            connected[a.index()][b.index()] = true;
            connected[b.index()][a.index()] = true;
        }
        SectionVisibility { connected }
    }
}

/// Vanilla's sparse-section threshold. A section with fewer than this many
/// opaque cells is assumed fully connected without flooding: with so few
/// occluders it almost always is, and the rare over-connection only ever draws
/// a section that could have been culled — never wrongly culls one. This is the
/// common case, since most sections in a loaded world are mostly air.
pub const SPARSE_OPAQUE_MAX: usize = 256;

/// Compute the face connectivity of a section by flooding its non-opaque cells.
///
/// Two shortcuts skip the flood entirely, matching vanilla's `VisGraph`:
/// a fully opaque section connects nothing ([`SectionVisibility::NONE`]), and a
/// section with fewer than [`SPARSE_OPAQUE_MAX`] opaque cells is treated as
/// fully connected ([`SectionVisibility::all`]). Everything in between floods.
#[must_use]
pub fn compute_visibility(section: &dyn SectionView) -> SectionVisibility {
    compute_visibility_from(|x, y, z| section.cell(x, y, z).occludes)
}

/// [`compute_visibility`] over a bare opacity predicate rather than a
/// [`SectionView`].
///
/// The producer side of the graph lives in the mesh worker, which already has a
/// cheap `(x,y,z) -> bool` occlusion lookup (`SnapshotModelView::occludes_at`,
/// vanilla's `isSolidRender` family) and no `SectionView` at all — resolving one
/// would mean resolving a sprite id and a light level per cell to throw both
/// away. Erring toward "not opaque" only ever *connects* more faces, which only
/// ever draws more; that is the safe direction and why the mesher's face-culling
/// predicate is a legitimate stand-in here.
#[must_use]
pub fn compute_visibility_from(
    opaque: impl Fn(usize, usize, usize) -> bool,
) -> SectionVisibility {
    let n = SECTION_SIZE;
    let total = n * n * n;

    // Single cheap pass: count opaque cells to pick a shortcut. Most sections
    // are mostly air and never reach the flood below.
    let mut opaque_count = 0usize;
    for x in 0..n {
        for y in 0..n {
            for z in 0..n {
                if opaque(x, y, z) {
                    opaque_count += 1;
                }
            }
        }
    }
    if opaque_count >= total {
        return SectionVisibility::solid(); // fully solid: skip flood
    }
    if opaque_count < SPARSE_OPAQUE_MAX {
        return SectionVisibility::all(); // sparse (incl. empty): skip flood
    }

    let idx = |x: usize, y: usize, z: usize| (x * n + y) * n + z;

    // Union-find over non-opaque cells.
    let mut parent: Vec<usize> = (0..n * n * n).collect();

    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent[ra] = rb;
        }
    }

    for x in 0..n {
        for y in 0..n {
            for z in 0..n {
                if opaque(x, y, z) {
                    continue;
                }
                // Union with the +X, +Y, +Z neighbours if also open.
                if x + 1 < n && !opaque(x + 1, y, z) {
                    union(&mut parent, idx(x, y, z), idx(x + 1, y, z));
                }
                if y + 1 < n && !opaque(x, y + 1, z) {
                    union(&mut parent, idx(x, y, z), idx(x, y + 1, z));
                }
                if z + 1 < n && !opaque(x, y, z + 1) {
                    union(&mut parent, idx(x, y, z), idx(x, y, z + 1));
                }
            }
        }
    }

    // For each face, collect the set of region roots touching it.
    let mut face_regions: [HashSet<usize>; 6] = Default::default();
    let last = n - 1;
    for a in 0..n {
        for b in 0..n {
            let mut touch = |x: usize, y: usize, z: usize, face: Face| {
                if !opaque(x, y, z) {
                    let r = find(&mut parent, idx(x, y, z));
                    face_regions[face.index()].insert(r);
                }
            };
            touch(0, a, b, Face::NegX);
            touch(last, a, b, Face::PosX);
            touch(a, 0, b, Face::NegY);
            touch(a, last, b, Face::PosY);
            touch(a, b, 0, Face::NegZ);
            touch(a, b, last, Face::PosZ);
        }
    }

    let mut connected = [[false; 6]; 6];
    for i in 0..6 {
        connected[i][i] = true;
        for j in (i + 1)..6 {
            let shared = face_regions[i]
                .intersection(&face_regions[j])
                .next()
                .is_some();
            connected[i][j] = shared;
            connected[j][i] = shared;
        }
    }
    SectionVisibility { connected }
}

/// A loaded graph of sections and their connectivity, for the camera walk.
#[derive(Debug, Clone, Default)]
pub struct VisibilityGraph {
    sections: HashMap<SectionCoord, SectionVisibility>,
    /// Bumped on every insert and every real removal, so a consumer caching a
    /// walk can tell "the camera has not moved and neither has the world" from
    /// "the world changed under me". See [`generation`](Self::generation).
    generation: u64,
    /// Inclusive section-grid row range covering every coord ever inserted.
    /// **Monotonically widened, never narrowed** — see [`y_extent`](Self::y_extent).
    y_extent: Option<(i32, i32)>,
}

impl VisibilityGraph {
    /// An empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a section's visibility.
    pub fn insert(&mut self, coord: SectionCoord, vis: SectionVisibility) {
        self.sections.insert(coord, vis);
        self.generation = self.generation.wrapping_add(1);
        self.y_extent = Some(match self.y_extent {
            Some((lo, hi)) => (lo.min(coord.1), hi.max(coord.1)),
            None => (coord.1, coord.1),
        });
    }

    /// Remove a section (e.g. on chunk unload). Returns its visibility if it was
    /// present, so callers can tell a real eviction from a no-op.
    pub fn remove(&mut self, coord: SectionCoord) -> Option<SectionVisibility> {
        let previous = self.sections.remove(&coord);
        if previous.is_some() {
            self.generation = self.generation.wrapping_add(1);
        }
        previous
    }

    /// How many times this graph has changed. The invalidation key for a cached
    /// [`walk_visible_bounded`] result, alongside the camera's 8-block cell.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A section's connectivity, if it is loaded.
    #[must_use]
    pub fn get(&self, coord: SectionCoord) -> Option<SectionVisibility> {
        self.sections.get(&coord).copied()
    }

    /// The inclusive section-grid row range to walk, or `None` for an empty
    /// graph. This is the vertical half of [`walk_visible_bounded`]'s bounds.
    ///
    /// **It only ever widens.** Recomputing it on removal would mean a scan of
    /// every key on every chunk unload, and a too-wide range costs a few extra
    /// all-air rows in the walk (which *draws more*, never less) and converges
    /// on the dimension's real height within one column's worth of inserts
    /// anyway — the overworld is 24 rows, full stop.
    #[must_use]
    pub fn y_extent(&self) -> Option<(i32, i32)> {
        self.y_extent
    }

    /// Whether a section is loaded.
    #[must_use]
    pub fn contains(&self, coord: SectionCoord) -> bool {
        self.sections.contains_key(&coord)
    }

    /// Number of loaded sections.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sections.len()
    }

    /// Whether the graph is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }
}

const fn step(coord: SectionCoord, face: Face) -> SectionCoord {
    let n = face.normal();
    (coord.0 + n[0], coord.1 + n[1], coord.2 + n[2])
}

/// One section in the BFS: which faces it has been *entered* through so far, and
/// which exits are still allowed by the never-reverse-an-axis rule.
#[derive(Debug, Clone, Copy)]
struct WalkNode {
    /// Every face this section has been reached through, **accumulated** across
    /// all paths that reached it before it was dequeued. This is vanilla's
    /// `Node.sourceDirections` and the reason it is a set rather than one face is
    /// recorded on [`walk_visible`].
    source_faces: [bool; 6],
    /// Exits still permitted: travelling along an axis forbids ever reversing
    /// along it, so the BFS only expands outward (vanilla's `Node.directions`).
    exits: [bool; 6],
    /// The camera's own section passes the connectivity gate unconditionally —
    /// you can see out of the section you are standing in in every direction,
    /// whatever its geometry says.
    is_camera: bool,
}

/// Walk the section graph from the camera, returning the sections that are
/// actually reachable through connected open space and pass `in_frustum`.
///
/// `in_frustum(coord)` lets the caller compose frustum culling; pass `|_| true`
/// to disable it. The camera's own section is always included (you can always
/// see the section you are standing in).
///
/// # Why the entry face is a *set*
///
/// The obvious implementation visits each section once, remembering the single
/// face it was first entered through, and then requires that face to connect to
/// each exit. **That over-culls**, and it is the "terrain disappears at certain
/// angles" bug class: a section reachable through both face B and face C, first
/// visited through C, loses every exit that only B connects to — and which of B
/// or C is "first" depends on `Face::ALL`'s order and on the camera's position,
/// so the missing geometry appears and vanishes as you turn.
///
/// Vanilla does not do that. `SectionOcclusionGraph.addNeighbors`
/// (`.cache/mc/26.2/client-src/net/minecraft/client/renderer/SectionOcclusionGraph.java:342-344`)
/// merges the direction into an already-created node —
///
/// ```java
/// } else if (existingNode != null) {
///     existingNode.addSourceDirection(direction);
/// }
/// ```
///
/// — and its visibility test (`:288-301`) passes a neighbour if **any**
/// accumulated source face connects to the exit. This function does the same:
/// `source_faces` is a 6-bit set, merged on re-reach, and read at *dequeue* time
/// so every path that arrived first has already contributed.
///
/// Like vanilla, an already-**dequeued** node is not re-expanded when a new source
/// face arrives; `exits` also comes from the first reach only
/// (`node1.setDirections(node.directions, direction)`). Both are vanilla's own
/// approximations, kept deliberately: diverging from them would make our cull
/// differ from the client we are matching, in the direction of drawing *fewer*
/// sections than vanilla does, which is the direction that loses pixels.
#[must_use]
pub fn walk_visible(
    graph: &VisibilityGraph,
    camera: SectionCoord,
    in_frustum: impl Fn(SectionCoord) -> bool,
) -> Vec<SectionCoord> {
    if !graph.contains(camera) {
        return Vec::new();
    }
    walk_visible_bounded(graph, camera, |coord| {
        graph.contains(coord) && in_frustum(coord)
    })
}

/// [`walk_visible`], but the graph is allowed to be **sparse**: a coord inside
/// `in_bounds` with no entry is treated as fully open
/// ([`SectionVisibility::all`]) rather than as a wall.
///
/// # Why this exists, and why the strict version is the trap
///
/// [`walk_visible`] stops at any coord the graph does not hold, and **all-air
/// sections are never meshed** — the shell's `snapshot_section_in` returns
/// `SnapshotOutcome::Empty` for them, so they never reach a mesh worker and can
/// have no computed connectivity. A graph built only from meshed sections
/// therefore dies at the first air gap above the terrain, the walk returns a
/// handful of coords, the reachable set is discarded as degenerate, and the cull
/// silently runs as pure frustum ∩ distance **forever**: it looks like it works
/// and costs exactly what it cost before.
///
/// Treating an absent in-bounds coord as air is not a workaround, it is vanilla's
/// own model: `ViewArea` holds a `SectionRenderDispatcher.RenderSection` for
/// *every* section in the render-distance cylinder regardless of content, and
/// `SectionOcclusionGraph` walks those. An all-air section there has an empty
/// `VisibilitySet`, which is `all()` here.
///
/// `in_bounds` **must be finite** — this walk has no node cap and its only
/// termination condition is running out of in-bounds coords. The production
/// caller passes the render-distance cylinder (vanilla's circular view
/// membership × the graph's [`y_extent`](VisibilityGraph::y_extent)).
///
/// The frustum is deliberately *not* passed here in production: reachability is
/// cached across frames and re-walked only on an 8-block camera-cell crossing or
/// a graph change (vanilla's `invalidateIfNeeded`), while the frustum is applied
/// per frame over the cached set. Folding the frustum in would make every mouse
/// movement a re-walk.
#[must_use]
pub fn walk_visible_bounded(
    graph: &VisibilityGraph,
    camera: SectionCoord,
    in_bounds: impl Fn(SectionCoord) -> bool,
) -> Vec<SectionCoord> {
    let mut result = Vec::new();
    let mut nodes: HashMap<SectionCoord, WalkNode> = HashMap::new();
    nodes.insert(
        camera,
        WalkNode {
            source_faces: [false; 6],
            exits: [true; 6],
            is_camera: true,
        },
    );
    result.push(camera);

    let mut queue: VecDeque<SectionCoord> = VecDeque::new();
    queue.push_back(camera);

    while let Some(coord) = queue.pop_front() {
        let node = nodes[&coord];
        // An in-bounds coord with no entry is air — see this function's doc.
        let vis = graph.get(coord).unwrap_or_else(SectionVisibility::all);
        for face in Face::ALL {
            // Never reverse along an axis we've already travelled.
            if !node.exits[face.index()] {
                continue;
            }
            // Connectivity gate: can we pass from *any* face we entered through
            // to this exit face? See this function's doc for why "any" rather
            // than "the first one".
            if !node.is_camera
                && !Face::ALL
                    .iter()
                    .any(|entry| node.source_faces[entry.index()] && vis.connects(*entry, face))
            {
                continue;
            }
            let next = step(coord, face);
            if !in_bounds(next) {
                continue;
            }
            let entry = face.opposite();
            if let Some(existing) = nodes.get_mut(&next) {
                // Merge, do not skip: this is the fidelity fix.
                existing.source_faces[entry.index()] = true;
                continue;
            }
            let mut exits = node.exits;
            exits[entry.index()] = false;
            let mut source_faces = [false; 6];
            source_faces[entry.index()] = true;
            nodes.insert(
                next,
                WalkNode {
                    source_faces,
                    exits,
                    is_camera: false,
                },
            );
            result.push(next);
            queue.push_back(next);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section::{Cell, SpriteId};

    struct Empty;
    impl SectionView for Empty {
        fn cell(&self, _x: usize, _y: usize, _z: usize) -> Cell {
            Cell::EMPTY
        }
    }
    struct Solid;
    impl SectionView for Solid {
        fn cell(&self, _x: usize, _y: usize, _z: usize) -> Cell {
            Cell::solid(SpriteId(1))
        }
    }
    /// A wall splitting the section in the X direction: connects Y/Z faces on
    /// each side but not across X. Concretely, solid at x==8.
    struct Wall;
    impl SectionView for Wall {
        fn cell(&self, x: usize, _y: usize, _z: usize) -> Cell {
            if x == 8 {
                Cell::solid(SpriteId(1))
            } else {
                Cell::EMPTY
            }
        }
    }
    /// A straight tube along Z: open only where x==8 && y==8.
    struct Tube;
    impl SectionView for Tube {
        fn cell(&self, x: usize, y: usize, _z: usize) -> Cell {
            if x == 8 && y == 8 {
                Cell::EMPTY
            } else {
                Cell::solid(SpriteId(1))
            }
        }
    }

    #[test]
    fn empty_section_connects_all_faces() {
        let v = compute_visibility(&Empty);
        for a in Face::ALL {
            for b in Face::ALL {
                assert!(v.connects(a, b));
            }
        }
    }

    #[test]
    fn solid_section_connects_only_self() {
        let v = compute_visibility(&Solid);
        for a in Face::ALL {
            for b in Face::ALL {
                assert_eq!(v.connects(a, b), a == b);
            }
        }
    }

    #[test]
    fn tube_connects_only_its_axis() {
        // Open cells only at x==8,y==8 for all z → NegZ connects PosZ, nothing
        // else (the tube never touches the X or Y faces).
        let v = compute_visibility(&Tube);
        assert!(v.connects(Face::NegZ, Face::PosZ));
        assert!(!v.connects(Face::NegX, Face::PosX));
        assert!(!v.connects(Face::NegY, Face::PosY));
        assert!(!v.connects(Face::NegZ, Face::NegX));
    }

    #[test]
    fn wall_blocks_across_x_but_connects_within() {
        let v = compute_visibility(&Wall);
        // The two open slabs each span full Y and Z, so within a slab the Y and
        // Z faces connect; but NegX and PosX are on opposite slabs → not
        // connected across the wall.
        assert!(v.connects(Face::NegY, Face::PosY));
        assert!(v.connects(Face::NegZ, Face::PosZ));
        assert!(!v.connects(Face::NegX, Face::PosX));
    }

    /// A partial X-wall of 240 opaque cells (`x==8`, `z<15`): fewer than the
    /// 256-cell threshold, so the sparse shortcut returns fully connected.
    struct SparseWall;
    impl SectionView for SparseWall {
        fn cell(&self, x: usize, _y: usize, z: usize) -> Cell {
            if x == 8 && z < 15 {
                Cell::solid(SpriteId(1))
            } else {
                Cell::EMPTY
            }
        }
    }

    #[test]
    fn sparse_section_shortcuts_to_fully_connected() {
        // 240 < SPARSE_OPAQUE_MAX, so the flood is skipped and all faces are
        // reported connected. This is also *correct*: a full separating plane
        // of a 16³ cube is 256 cells (the min cut between opposite faces), so
        // fewer than 256 opaque cells can never disconnect a face pair. The
        // shortcut is safe, not merely conservative.
        let v = compute_visibility(&SparseWall);
        for a in Face::ALL {
            for b in Face::ALL {
                assert!(v.connects(a, b), "{a:?}<->{b:?} should be connected");
            }
        }
    }

    #[test]
    fn fully_solid_section_shortcuts_to_diagonal_only() {
        // Solid → self-connections only, without flooding.
        let v = compute_visibility(&Solid);
        for a in Face::ALL {
            for b in Face::ALL {
                assert_eq!(v.connects(a, b), a == b);
            }
        }
    }

    #[test]
    fn dense_wall_at_threshold_still_floods_and_disconnects() {
        // The full-plane Wall is exactly 256 opaque cells — not below the
        // threshold — so it takes the real flood path and correctly reports
        // NegX/PosX disconnected. Guards the boundary of the shortcut.
        let v = compute_visibility(&Wall);
        assert!(!v.connects(Face::NegX, Face::PosX));
    }

    #[test]
    fn walk_stops_at_solid_sections() {
        // Camera in an open section; a solid section sits between it and a
        // farther open section along +X. The far section must be culled.
        let mut g = VisibilityGraph::new();
        g.insert((0, 0, 0), SectionVisibility::all());
        g.insert((1, 0, 0), SectionVisibility::NONE); // solid blocker
        g.insert((2, 0, 0), SectionVisibility::all());
        let visible = walk_visible(&g, (0, 0, 0), |_| true);
        assert!(visible.contains(&(0, 0, 0)));
        assert!(visible.contains(&(1, 0, 0))); // adjacent, still drawn
        assert!(!visible.contains(&(2, 0, 0)), "behind a solid section");
    }

    #[test]
    fn walk_follows_a_connected_corridor() {
        let mut g = VisibilityGraph::new();
        for x in 0..4 {
            g.insert((x, 0, 0), SectionVisibility::all());
        }
        let visible = walk_visible(&g, (0, 0, 0), |_| true);
        assert_eq!(visible.len(), 4);
    }

    #[test]
    fn walk_respects_frustum_predicate() {
        let mut g = VisibilityGraph::new();
        for x in 0..4 {
            g.insert((x, 0, 0), SectionVisibility::all());
        }
        // Cull everything past x==1 with the frustum predicate.
        let visible = walk_visible(&g, (0, 0, 0), |c| c.0 <= 1);
        assert!(visible.contains(&(1, 0, 0)));
        assert!(!visible.contains(&(2, 0, 0)));
    }

    #[test]
    fn walk_does_not_pass_through_a_disconnected_section() {
        // A section that connects NegX↔PosX but nothing to +Y: a branch upward
        // must not be taken through it.
        let mut connected = SectionVisibility::NONE;
        connected.connected[Face::NegX.index()][Face::PosX.index()] = true;
        connected.connected[Face::PosX.index()][Face::NegX.index()] = true;
        let mut g = VisibilityGraph::new();
        g.insert((0, 0, 0), SectionVisibility::all());
        g.insert((1, 0, 0), connected);
        g.insert((1, 1, 0), SectionVisibility::all()); // above the middle
        g.insert((2, 0, 0), SectionVisibility::all());
        let visible = walk_visible(&g, (0, 0, 0), |_| true);
        assert!(visible.contains(&(2, 0, 0)), "straight-through is allowed");
        assert!(
            !visible.contains(&(1, 1, 0)),
            "the middle section does not connect +X entry to +Y exit"
        );
    }

    /// The walk-fidelity fixture: a corridor section reachable through **two**
    /// faces, whose only exit connects to the one the BFS reaches *second*.
    ///
    /// This is the over-cull the pre-merge `walk_visible` had, and it is the
    /// "geometry disappears at certain angles" class: which of the two faces is
    /// first depends on `Face::ALL`'s order, so the old code lost the far section
    /// for one of the two arms below and not the other. Both arms are here for
    /// exactly that reason — a single arm would pass under the old code half the
    /// time, and which half is an implementation detail of an unrelated array.
    #[test]
    fn walk_merges_source_faces_reached_by_a_second_path() {
        // Arm 1: the corridor at (1,0,1) connects only the X axis, and the target
        // is further along +X. It is reachable via (1,0,0) (entering through NegZ,
        // which connects to nothing useful) and via (0,0,1) (entering through
        // NegX, which does connect to PosX).
        let corridor_x = SectionVisibility::from_pairs(&[(Face::NegX, Face::PosX)]);
        let mut g = VisibilityGraph::new();
        g.insert((0, 0, 0), SectionVisibility::all());
        g.insert((1, 0, 0), SectionVisibility::all());
        g.insert((0, 0, 1), SectionVisibility::all());
        g.insert((1, 0, 1), corridor_x);
        g.insert((2, 0, 1), SectionVisibility::all());
        let visible = walk_visible(&g, (0, 0, 0), |_| true);
        assert!(
            visible.contains(&(2, 0, 1)),
            "the X corridor is entered through NegX by the (0,0,1) path, which connects to \
             PosX, so (2,0,1) is visible however the BFS ordered the two paths"
        );

        // Arm 2: the mirror image — the corridor connects only the Z axis and the
        // target is further along +Z.
        let corridor_z = SectionVisibility::from_pairs(&[(Face::NegZ, Face::PosZ)]);
        let mut g = VisibilityGraph::new();
        g.insert((0, 0, 0), SectionVisibility::all());
        g.insert((1, 0, 0), SectionVisibility::all());
        g.insert((0, 0, 1), SectionVisibility::all());
        g.insert((1, 0, 1), corridor_z);
        g.insert((1, 0, 2), SectionVisibility::all());
        let visible = walk_visible(&g, (0, 0, 0), |_| true);
        assert!(
            visible.contains(&(1, 0, 2)),
            "mirror image of arm 1: the Z corridor is entered through NegZ by the (1,0,0) path"
        );
    }

    /// The trap this whole `_bounded` variant exists for, stated as a test:
    /// a graph holding only *meshed* sections, with an air gap between the camera
    /// and the terrain. The strict walk dies at the gap; the bounded one crosses
    /// it and still stops at the solid section.
    #[test]
    fn bounded_walk_crosses_an_unmeshed_air_gap_and_the_strict_one_does_not() {
        // Terrain at x==3 (solid, meshed); x==1 and x==2 are all-air and hence
        // absent from the graph entirely. Camera at x==0, which is also air.
        let mut g = VisibilityGraph::new();
        g.insert((0, 0, 0), SectionVisibility::all());
        g.insert((3, 0, 0), SectionVisibility::NONE);
        g.insert((4, 0, 0), SectionVisibility::all());

        let bounds = |c: SectionCoord| (0..=4).contains(&c.0) && c.1 == 0 && c.2 == 0;
        let visible = walk_visible_bounded(&g, (0, 0, 0), bounds);
        assert!(visible.contains(&(1, 0, 0)), "the air gap is reachable");
        assert!(visible.contains(&(2, 0, 0)));
        assert!(visible.contains(&(3, 0, 0)), "the terrain itself is drawn");
        assert!(
            !visible.contains(&(4, 0, 0)),
            "the solid section still blocks — treating air as open must not \
             open a path *through* real geometry"
        );

        // The control: the same graph under the strict walk reaches the camera's
        // own section and nothing else, which is exactly the silent degradation
        // (`reachable` too small to be believed → fall back to pure frustum).
        let strict = walk_visible(&g, (0, 0, 0), |_| true);
        assert_eq!(strict, vec![(0, 0, 0)]);
    }

    #[test]
    fn generation_and_y_extent_track_inserts() {
        let mut g = VisibilityGraph::new();
        assert_eq!(g.y_extent(), None);
        let g0 = g.generation();
        g.insert((0, -4, 0), SectionVisibility::all());
        g.insert((0, 19, 0), SectionVisibility::all());
        assert_eq!(g.y_extent(), Some((-4, 19)));
        assert_ne!(g.generation(), g0);
        // A removal bumps the generation; a no-op removal does not.
        let g1 = g.generation();
        assert!(g.remove((0, 19, 0)).is_some());
        assert_ne!(g.generation(), g1);
        let g2 = g.generation();
        assert!(g.remove((7, 7, 7)).is_none());
        assert_eq!(g.generation(), g2);
        // Monotonic by design: still (-4, 19) after the removal.
        assert_eq!(g.y_extent(), Some((-4, 19)));
    }

    #[test]
    fn compute_visibility_from_matches_the_section_view_form() {
        // Same wall, expressed as a predicate rather than a `SectionView`.
        let from_view = compute_visibility(&Wall);
        let from_pred = compute_visibility_from(|x, _y, _z| x == 8);
        for a in Face::ALL {
            for b in Face::ALL {
                assert_eq!(from_view.connects(a, b), from_pred.connects(a, b));
            }
        }
        assert!(!from_pred.connects(Face::NegX, Face::PosX));
    }

    #[test]
    fn from_pairs_is_symmetric_and_self_connected() {
        let v = SectionVisibility::from_pairs(&[(Face::NegX, Face::PosY)]);
        assert!(v.connects(Face::NegX, Face::PosY));
        assert!(v.connects(Face::PosY, Face::NegX));
        for f in Face::ALL {
            assert!(v.connects(f, f));
        }
        assert!(!v.connects(Face::NegX, Face::PosX));
    }
}
