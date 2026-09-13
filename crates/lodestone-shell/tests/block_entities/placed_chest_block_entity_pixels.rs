//! Pixel gate: a chest set through the **block-update path** must reach pixels,
//! and removing it must make the draw disappear — and the same must
//! hold for a chest the client **predicts locally**, with no server packet at all,
//! including when the server then refuses the placement.
//!
//! Three gates live here, in the order the two issues arrived:
//!
//! | gate | route | control |
//! |---|---|---|
//! | [`a_chest_set_by_a_block_update_reaches_pixels_and_stops_when_removed`] | `BLOCK_UPDATE`'s `set_block` + `sync_block_entity` | the state write with no block-entity half, i.e. That fix itself |
//! | [`a_locally_predicted_chest_reaches_pixels_with_no_server_packet`] | `lodestone::sim::write_predicted_block`, the production prediction | no local write at all, i.e. That fix itself |
//! | [`a_refused_placement_loses_the_predicted_block_entity`] | predict, then the server's correction | a world that never had a chest |
//!
//! # What this gate covers that `chest_block_entity_pixels.rs` does not
//!
//! That gate hands `RenderState` a synthetic `ChestSpawn` closure. It proved the
//! *render* half of that fix and is silent about where spawns come from — which is
//! exactly where that fix lived: `block_update` wrote a block state and nothing else,
//! so a freshly placed chest had a state, **no block-entity record**, and
//! `block_entities::chest_candidates`' `for be in &chunk.block_entities` loop
//! never saw it. It drew zero pixels and still *opened*, because interaction
//! resolves from the block state.
//!
//! So this gate starts one layer earlier: a real [`lodestone_world::World`] with
//! a real loaded chunk, written through the [`WorldSink`] seam — `set_block`
//! followed by `sync_block_entity`, which is exactly the pair the v26-2 adapter's
//! `BLOCK_UPDATE` arm calls (`crates/versions/26.2/tests/block_updates.rs`
//! dispatches the real packet bytes into a real `World` and asserts the resulting
//! records, so that link is proved there rather than assumed here). It then runs
//! the **real** shell gather — `chest_candidates` + `chest_spawn` — and renders
//! the result through the real [`RenderState::render`].
//!
//! # Three frames, and why the negative control is the *pre-fix* code path
//!
//! | frame | world write | expectation |
//! |---|---|---|
//! | subject | `set_block(chest)` + `sync_block_entity(chest type)` | chest fills its rect |
//! | pre-fix control | `set_block(chest)` **only** | zero spawns, zero px in the rect |
//! | removed | then `set_block(air)` + `sync_block_entity(None)` | back to zero px |
//!
//! The middle row is that fix itself, reproduced verbatim: the old adapter arm was
//! `world.set_block(pos.x, pos.y, pos.z, state);` with no second call. It is kept
//! as a permanent control rather than described, because "the chest reaches
//! pixels" is only meaningful next to a run where it provably does not — and this
//! is the one shape of that run which cannot rot, since it is a *world state*, not
//! a deleted line of code.
//!
//! The third row is the other direction, and it matters as much: a stale record
//! would keep drawing a chest in empty air after the block is broken.
//!
//! # The rect, and what else paints there
//!
//! The rect is projected from the **real baked vertices** of the mesh the draw
//! resolves, through the same `Camera::view_projection` and the same
//! `part_transforms` the draw uses — and from the spawn *this gate's own gather
//! produced*, not from a hand-built `ChestSpawn`, so a wrong facing or half shows
//! up as a mismatched rect instead of being papered over.
//!
//! `CLAUDE.md` records a control that asserted a frame "clears uniformly" and
//! failed at 3.5% because of the unconditional **first-person bare arm**, and the
//! That fix chest gate's first failure bbox landed on that same arm. So
//! [`the_first_person_arm_is_disjoint_from_the_chest_rect`] *locates* the arm and
//! asserts disjointness rather than assuming the rect is clean. Failure output
//! prints bounding boxes, never a bare percentage.
//!
//! Fail-closed: no GPU adapter or no `client.jar` is a **failure, never a skip**.
//! A chest with no sheet draws nothing rather than a placeholder, so a jar-less
//! run would otherwise be indistinguishable from a pass.
//!
//! ```text
//! cargo test -p lodestone-shell --test placed_chest_block_entity_pixels -- --ignored --nocapture
//! ```

use lodestone::block_entities::{ChestLids, chest_candidates, chest_spawn};
use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone::sim::{predicted_placement_state, write_predicted_block};
use lodestone_game::placement::PlacedState;
use lodestone_model::BlockFace;
use lodestone_render::{
    BlockEntityMesh, BlockEntityModelSet, Camera, ChestSpawn, ENTITY_FULLBRIGHT, GpuContext,
    HeadlessTarget, RenderTarget,
};
use lodestone_world::{
    BlockEntitySync, ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind,
    World, WorldSink,
};

const W: u32 = 320;
const H: u32 = 240;

/// The chest's block position — the same one `chest_block_entity_pixels.rs` uses,
/// so the arm-disjointness measurement carries over and can be re-checked here.
const CHEST: [i32; 3] = [0, 0, 4];

/// Manhattan RGB distance above which a pixel counts as "not the clear colour".
const NON_SKY: i32 = 60;

/// Every block-entity sheet the loader asks the jar for, so a silently jar-less run
/// cannot pass by drawing nothing.
///
/// **Derived, not a literal** — see `chest_block_entity_pixels.rs`'s copy for why.
/// This was `22` (chests only) and went stale the moment the skull renderer added
/// sheets to the same loader, failing a chest gate for a skull's reason.
fn expected_sheets() -> usize {
    lodestone_render::block_entity::block_entity_texture_stems().len()
}

fn sky_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
}

fn is_non_sky(px: &[u8], sky: [u8; 3]) -> bool {
    let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
        + (i32::from(px[1]) - i32::from(sky[1])).abs()
        + (i32::from(px[2]) - i32::from(sky[2])).abs();
    d > NON_SKY
}

/// An inclusive pixel rect, in screen space.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    fn area(self) -> usize {
        ((self.x1 - self.x0 + 1) as usize) * ((self.y1 - self.y0 + 1) as usize)
    }

    fn contains(self, x: u32, y: u32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    fn padded(self, pad: u32) -> Rect {
        Rect {
            x0: self.x0.saturating_sub(pad),
            y0: self.y0.saturating_sub(pad),
            x1: (self.x1 + pad).min(W - 1),
            y1: (self.y1 + pad).min(H - 1),
        }
    }

    fn intersects(self, other: Rect) -> bool {
        self.x0 <= other.x1 && other.x0 <= self.x1 && self.y0 <= other.y1 && other.y0 <= self.y1
    }
}

/// Bounding box of every pixel `predicate` accepts, plus the count.
fn bbox_of(pixels: &[u8], predicate: impl Fn(&[u8]) -> bool) -> Option<(Rect, usize)> {
    let mut rect: Option<Rect> = None;
    let mut count = 0usize;
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        if !predicate(px) {
            continue;
        }
        count += 1;
        let x = (i as u32) % W;
        let y = (i as u32) / W;
        rect = Some(grow(rect, x, y));
    }
    rect.map(|r| (r, count))
}

/// Bounding box of the pixels that differ between two frames.
fn changed_bbox(a: &[u8], b: &[u8]) -> Option<(Rect, usize)> {
    let mut rect: Option<Rect> = None;
    let mut count = 0usize;
    for (i, (pa, pb)) in a.chunks_exact(4).zip(b.chunks_exact(4)).enumerate() {
        let d = (i32::from(pa[0]) - i32::from(pb[0])).abs()
            + (i32::from(pa[1]) - i32::from(pb[1])).abs()
            + (i32::from(pa[2]) - i32::from(pb[2])).abs();
        if d <= 12 {
            continue;
        }
        count += 1;
        let x = (i as u32) % W;
        let y = (i as u32) / W;
        rect = Some(grow(rect, x, y));
    }
    rect.map(|r| (r, count))
}

fn grow(rect: Option<Rect>, x: u32, y: u32) -> Rect {
    match rect {
        None => Rect {
            x0: x,
            y0: y,
            x1: x,
            y1: y,
        },
        Some(r) => Rect {
            x0: r.x0.min(x),
            y0: r.y0.min(y),
            x1: r.x1.max(x),
            y1: r.y1.max(y),
        },
    }
}

fn non_sky_in(pixels: &[u8], rect: Rect, sky: [u8; 3]) -> usize {
    let mut n = 0;
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        let x = (i as u32) % W;
        let y = (i as u32) / W;
        if rect.contains(x, y) && is_non_sky(px, sky) {
            n += 1;
        }
    }
    n
}

fn project(view_proj: glam::Mat4, world: glam::Vec3) -> (f32, f32) {
    let clip = view_proj * glam::Vec4::new(world.x, world.y, world.z, 1.0);
    (
        (clip.x / clip.w * 0.5 + 0.5) * W as f32,
        (1.0 - (clip.y / clip.w * 0.5 + 0.5)) * H as f32,
    )
}

/// The screen rect of a posed mesh, projected from its real baked vertices
/// through the very `part_transforms` the draw uses. Every vertex individually,
/// not eight AABB corners, so the rect stays tight.
fn posed_screen_rect(
    mesh: &BlockEntityMesh,
    part_transforms: &[glam::Mat4],
    view_proj: glam::Mat4,
) -> Rect {
    let mut min = (f32::MAX, f32::MAX);
    let mut max = (f32::MIN, f32::MIN);
    for (index, range) in mesh.parts.iter().enumerate() {
        let start = range.vertex_start as usize;
        let end = start + range.vertex_count as usize;
        for vertex in &mesh.vertices[start..end] {
            let world = part_transforms[index].transform_point3(glam::Vec3::from(vertex.position));
            let (sx, sy) = project(view_proj, world);
            min = (min.0.min(sx), min.1.min(sy));
            max = (max.0.max(sx), max.1.max(sy));
        }
    }
    assert!(min.0 < max.0 && min.1 < max.1, "no vertices projected");
    Rect {
        x0: min.0.max(0.0).floor() as u32,
        y0: min.1.max(0.0).floor() as u32,
        x1: (max.0.min((W - 1) as f32)).ceil() as u32,
        y1: (max.1.min((H - 1) as f32)).ceil() as u32,
    }
}

/// Eye slightly above the chest's mid-height, four blocks back on `-Z`, looking
/// straight down `+Z` (yaw `0` faces `+Z` in Minecraft's convention).
fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, 0.45, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

fn gpu() -> GpuContext {
    GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    )
}

// ---------------------------------------------------------------------------
// The world fixture
// ---------------------------------------------------------------------------

/// The chunk column that owns [`CHEST`]. **Loaded**, deliberately and asserted:
/// `set_block`, `set_blocks` and `sync_block_entity` are all documented no-ops
/// for an absent chunk, so a fixture that forgot this would see no record and
/// read as a broken feature — the `world` species of vacuous test, invisible in
/// the test source.
fn world_with_chunk() -> (World, ChunkPos) {
    let pos = ChunkPos::new(CHEST[0] >> 4, CHEST[2] >> 4);
    let mut world = World::new();
    let column = ChunkColumn::new(
        -64,
        24,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        0,
        0,
    );
    world.load(
        pos,
        LoadedChunk::new(column, ColumnLight::new(26), Heightmaps::new(), Vec::new()),
    );
    assert!(world.get(pos).is_some(), "the fixture chunk must be loaded");
    (world, pos)
}

/// The first block state of a named block, from the real 26.2 census. Never a
/// hardcoded state id — those shift with every data bump.
fn first_state_named(name: &str) -> u32 {
    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} is not in the 26.2 block-state table"))
}

/// One block-state write through the `WorldSink` seam, `sync` selecting whether
/// the block-entity half runs.
///
/// `sync == false` reproduces the pre-fix adapter arm exactly: `set_block` and
/// nothing else.
fn write_block(world: &mut World, block: [i32; 3], state: u32, sync: bool) -> Option<BlockEntitySync> {
    let sink: &mut dyn WorldSink = world;
    sink.set_block(block[0], block[1], block[2], state);
    sync.then(|| {
        sink.sync_block_entity(
            block[0],
            block[1],
            block[2],
            lodestone_data::block_states::StateId::new(state)
                .and_then(lodestone_data::block_entity_types::block_entity_type)
                .map(|kind| kind.raw()),
        )
    })
}

/// The real shell gather: `chest_candidates` over the world, then `chest_spawn`
/// per candidate. This is the code path that fix starved of input.
fn gather(world: &World, pos: ChunkPos, eye: glam::Vec3) -> Vec<ChestSpawn> {
    let lids = ChestLids::new();
    chest_candidates(world, [pos], eye)
        .into_iter()
        .filter_map(|(block, state)| {
            lodestone_data::block_states::StateId::new(state).and_then(|state_id| {
                chest_spawn(
                    block,
                    state_id,
                    lids.openness(block, 1.0),
                    ENTITY_FULLBRIGHT,
                )
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_chest_set_by_a_block_update_reaches_pixels_and_stops_when_removed() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();
    let eye = camera.position;

    let chest_state = first_state_named("minecraft:chest");
    let air_state = first_state_named("minecraft:air");
    println!(
        "chest state {chest_state} props {:?}; air state {air_state}",
        lodestone_data::block_states::properties(chest_state)
    );

    // --- Subject: the block-update path, both halves. ------------------------
    let (mut world, pos) = world_with_chunk();
    let outcome = write_block(&mut world, CHEST, chest_state, true);
    assert_eq!(
        outcome,
        Some(BlockEntitySync::Created),
        "the state write must have created a record; `ChunkAbsent` here would mean \
         the fixture chunk is not loaded and everything below is vacuous"
    );
    let subject_spawns = gather(&world, pos, eye);
    assert_eq!(
        subject_spawns.len(),
        1,
        "the real shell gather must find exactly one chest, got {subject_spawns:?}"
    );

    // --- The pre-fix control: the same state write, block-entity half absent.
    let (mut pre_fix_world, pre_fix_pos) = world_with_chunk();
    assert_eq!(write_block(&mut pre_fix_world, CHEST, chest_state, false), None);
    assert_eq!(
        pre_fix_world
            .get(pre_fix_pos)
            .expect("chunk")
            .column
            .get_block(
                (CHEST[0] & 15) as usize,
                CHEST[1],
                (CHEST[2] & 15) as usize,
            ),
        chest_state,
        "the control's state write must have landed — otherwise it is measuring an \
         empty world, not the #374 bug"
    );
    let pre_fix_spawns = gather(&pre_fix_world, pre_fix_pos, eye);
    assert!(
        pre_fix_spawns.is_empty(),
        "a block state with no block-entity record must yield no spawns — that is \
         issue #374, and if this list is non-empty the control is not reproducing it: \
         {pre_fix_spawns:?}"
    );

    // --- The removal direction: break the chest. -----------------------------
    let mut removed_world = world.clone();
    assert_eq!(
        write_block(&mut removed_world, CHEST, air_state, true),
        Some(BlockEntitySync::Removed),
        "air owns no block entity, so the record must be dropped"
    );
    let removed_spawns = gather(&removed_world, pos, eye);
    assert!(
        removed_spawns.is_empty(),
        "a broken chest must stop being gathered: {removed_spawns:?}"
    );

    // --- The rect, from the spawn this gate's own gather produced. -----------
    let models = BlockEntityModelSet::load();
    let spawn = subject_spawns[0];
    println!("gathered spawn: {spawn:?}");
    assert_eq!(spawn.pos, CHEST, "the record must be keyed by the block written");
    let instance = models
        .resolve_chest(&spawn)
        .expect("the gathered chest must resolve to a model in the corpus");
    let mesh = models.get(instance.model).expect("mesh");
    let chest_rect = posed_screen_rect(mesh, &instance.part_transforms, camera.view_projection());
    println!("chest rect (from real baked vertices): {chest_rect:?}");
    assert!(
        chest_rect.area() > 900,
        "the chest projects to only {} px — the camera, not the renderer, is wrong: \
         {chest_rect:?}",
        chest_rect.area()
    );

    // --- Render all three. ---------------------------------------------------
    let mut shoot = |spawns: Vec<ChestSpawn>| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        state.set_block_entity_source(move |_eye| spawns.clone());
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats)
    };

    let (subject_px, subject_stats) = shoot(subject_spawns.clone());
    let (pre_fix_px, pre_fix_stats) = shoot(pre_fix_spawns);
    let (removed_px, removed_stats) = shoot(removed_spawns);

    // The vanilla pack really loaded; without this every "absent" assertion is
    // satisfiable by a renderer that draws nothing at all.
    assert_eq!(
        subject_stats.block_entity_sheets_loaded,
        expected_sheets(),
        "expected every block-entity sheet from client.jar"
    );
    assert_eq!(subject_stats.block_entities_drawn, 1);
    assert_eq!(subject_stats.block_entities_culled, 0);
    assert_eq!(
        pre_fix_stats.block_entities_drawn, 0,
        "the pre-#374 world has no record, so nothing can be drawn"
    );
    assert_eq!(removed_stats.block_entities_drawn, 0);

    let sky = sky_bytes();

    // --- Absolute, inside the rect. The control's premise, measured. ---------
    let pre_fix_in_rect = non_sky_in(&pre_fix_px, chest_rect, sky);
    assert_eq!(
        pre_fix_in_rect, 0,
        "the pre-fix control paints {pre_fix_in_rect} px inside the chest's own rect \
         {chest_rect:?} — something *else* draws there (the first-person arm?), so \
         this gate would be measuring that. Control's whole non-sky bbox: {:?}",
        bbox_of(&pre_fix_px, |px| is_non_sky(px, sky))
    );

    let subject_in_rect = non_sky_in(&subject_px, chest_rect, sky);
    let fill = subject_in_rect as f64 / chest_rect.area() as f64;
    println!(
        "rect {chest_rect:?} area {} — subject {subject_in_rect} px ({:.1}%), \
         pre-fix {pre_fix_in_rect} px",
        chest_rect.area(),
        fill * 100.0
    );
    assert!(
        fill > 0.45,
        "the chest fills only {:.1}% of its own projected rect {chest_rect:?} \
         ({subject_in_rect} of {} px). A closed chest is a solid box, so anything this \
         sparse means it drew partially, inside-out, or somewhere else. Subject's \
         non-sky bbox: {:?}",
        fill * 100.0,
        chest_rect.area(),
        bbox_of(&subject_px, |px| is_non_sky(px, sky))
    );

    // --- Differential: every changed pixel must *be* the chest. --------------
    let (changed_rect, changed_count) = changed_bbox(&subject_px, &pre_fix_px).expect(
        "creating the block entity from the block state changed no pixel at all — \
         the chain from sync_block_entity to the screen is dead",
    );
    println!("changed bbox {changed_rect:?} ({changed_count} px)");
    let allowed = chest_rect.padded(2);
    assert!(
        allowed.x0 <= changed_rect.x0
            && allowed.y0 <= changed_rect.y0
            && changed_rect.x1 <= allowed.x1
            && changed_rect.y1 <= allowed.y1,
        "pixels changed outside the chest's projected rect: changed {changed_rect:?}, \
         allowed {allowed:?}"
    );
    assert!(
        changed_count > chest_rect.area() / 2,
        "only {changed_count} px changed inside a {} px rect",
        chest_rect.area()
    );

    // --- The reverse, in pixels: the draw disappears. ------------------------
    let removed_in_rect = non_sky_in(&removed_px, chest_rect, sky);
    assert_eq!(
        removed_in_rect, 0,
        "after the block became air the chest still paints {removed_in_rect} px in \
         {chest_rect:?} — a stale block-entity record is drawing a chest in empty \
         air. Non-sky bbox of the removed frame: {:?}",
        bbox_of(&removed_px, |px| is_non_sky(px, sky))
    );
    assert!(
        changed_bbox(&removed_px, &pre_fix_px).is_none(),
        "the removed frame must be pixel-identical to a world that never had a \
         chest; it differs at {:?}",
        changed_bbox(&removed_px, &pre_fix_px)
    );
}

// ---------------------------------------------------------------------------
// The same thing, reached through the local prediction
// ---------------------------------------------------------------------------

/// The chest a right-click predicts must reach pixels with **no server packet at
/// all**, and be pixel-identical to the same chest delivered by `BLOCK_UPDATE`.
///
/// The gate above proves the *packet* route. That fix is the other one: `use_item_live`
/// used to send `use_item_on` and wait, so between the click and the server's reply
/// there was no local state write, therefore (since that fix) no local block-entity
/// record, therefore a hole where the chest should be. The fix is
/// [`write_predicted_block`], and this drives **that production function** rather
/// than re-spelling its two calls — a re-spelling would pass with the prediction
/// deleted, which is the island `CLAUDE.md`'s first rule is about.
///
/// The state it draws is the one the **resolver** picked, not one this file chose:
/// [`lodestone::sim::predicted_placement_state`] is the same call the click makes.
/// That matters because "a chest" is not one state — `minecraft:chest` has 24, and
/// the *lowest id* among them is a **waterlogged** chest (`BooleanProperty` orders
/// its values `{true, false}`), which would render as a plausible chest while being
/// the wrong block. So the properties of the resolved state are asserted against
/// `ChestBlock.getStateForPlacement` before any pixel is measured, and the frame is
/// then required to be identical to the same state delivered by `BLOCK_UPDATE` —
/// which proves the two write paths agree, the resolution itself being pinned to
/// `blocks.json` by `sim.rs`'s `placement_states_resolve_to_the_jar_oracle`.
///
/// ```text
/// cargo test -p lodestone-shell --test placed_chest_block_entity_pixels -- --ignored --nocapture
/// ```
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_locally_predicted_chest_reaches_pixels_with_no_server_packet() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();
    let eye = camera.position;

    // Yaw 0 looks down +Z (south) and a chest faces *away* from the player, so the
    // geometry a click at this camera resolves is `facing = north`.
    let chest_state = predicted_placement_state(
        "minecraft:chest",
        &PlacedState {
            facing: Some(BlockFace::North),
            ..PlacedState::default()
        },
    )
    .expect("the shell must be willing to predict a chest — that is issue #381");
    let properties = lodestone_data::block_states::properties(chest_state).expect("in census");
    println!("resolved chest state {chest_state} props {properties:?}");
    assert_eq!(
        properties,
        &[
            ("facing", "north"),
            ("type", "single"),
            ("waterlogged", "false"),
        ],
        "the predicted state must be what `ChestBlock.getStateForPlacement` yields: \
         the requested facing, `single` (no adjacent chest), and not waterlogged \
         (the target cell is air). Anything else still *draws* a chest, which is why \
         this is checked here rather than inferred from the pixels."
    );

    // --- Subject: the local prediction. No packet is decoded anywhere here. ---
    let (mut predicted_world, pos) = world_with_chunk();
    let outcome = write_predicted_block(&mut predicted_world, CHEST, chest_state);
    assert_eq!(
        outcome,
        BlockEntitySync::Created,
        "the prediction must create the record itself; `ChunkAbsent` would mean the \
         fixture chunk is not loaded and everything below is vacuous"
    );
    let predicted_spawns = gather(&predicted_world, pos, eye);
    assert_eq!(
        predicted_spawns.len(),
        1,
        "the real shell gather must find the predicted chest, got {predicted_spawns:?}"
    );

    // --- The pre-fix control: the click sent, nothing written locally. --------
    // This is not "the write with its second half removed" (that is that fix, gated
    // above) — it is the whole prediction absent, which is what `use_item_live`
    // did. A world state, so it cannot rot.
    let (unpredicted_world, unpredicted_pos) = world_with_chunk();
    let unpredicted_spawns = gather(&unpredicted_world, unpredicted_pos, eye);
    assert!(
        unpredicted_spawns.is_empty(),
        "the control must have no chest at all: {unpredicted_spawns:?}"
    );

    // --- The authoritative route, for the identity check. ---------------------
    let (mut updated_world, updated_pos) = world_with_chunk();
    write_block(&mut updated_world, CHEST, chest_state, true);
    let updated_spawns = gather(&updated_world, updated_pos, eye);
    assert_eq!(updated_spawns.len(), 1);

    // --- The rect, from the spawn this gate's own gather produced. ------------
    let models = BlockEntityModelSet::load();
    let spawn = predicted_spawns[0];
    println!("predicted spawn: {spawn:?}");
    assert_eq!(spawn.pos, CHEST, "the record must be keyed by the block written");
    let instance = models
        .resolve_chest(&spawn)
        .expect("the predicted chest must resolve to a model in the corpus");
    let mesh = models.get(instance.model).expect("mesh");
    let chest_rect = posed_screen_rect(mesh, &instance.part_transforms, camera.view_projection());
    println!("chest rect (from real baked vertices): {chest_rect:?}");
    assert!(
        chest_rect.area() > 900,
        "the chest projects to only {} px — the camera, not the renderer, is wrong: \
         {chest_rect:?}",
        chest_rect.area()
    );

    let mut shoot = |spawns: Vec<ChestSpawn>| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        state.set_block_entity_source(move |_eye| spawns.clone());
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats)
    };
    let (predicted_px, predicted_stats) = shoot(predicted_spawns);
    let (unpredicted_px, unpredicted_stats) = shoot(unpredicted_spawns);
    let (updated_px, _) = shoot(updated_spawns);

    assert_eq!(
        predicted_stats.block_entity_sheets_loaded,
        expected_sheets(),
        "expected every block-entity sheet from client.jar"
    );
    assert_eq!(predicted_stats.block_entities_drawn, 1);
    assert_eq!(unpredicted_stats.block_entities_drawn, 0);

    let sky = sky_bytes();

    // The control's premise, measured rather than assumed — the first-person arm
    // is the thing that has broken this class of control before.
    let control_in_rect = non_sky_in(&unpredicted_px, chest_rect, sky);
    assert_eq!(
        control_in_rect, 0,
        "the un-predicted control paints {control_in_rect} px inside the chest's own \
         rect {chest_rect:?} — something *else* draws there, so this gate would be \
         measuring that. Control's whole non-sky bbox: {:?}",
        bbox_of(&unpredicted_px, |px| is_non_sky(px, sky))
    );

    let predicted_in_rect = non_sky_in(&predicted_px, chest_rect, sky);
    let fill = predicted_in_rect as f64 / chest_rect.area() as f64;
    println!(
        "rect {chest_rect:?} area {} — predicted {predicted_in_rect} px ({:.1}%), \
         control {control_in_rect} px",
        chest_rect.area(),
        fill * 100.0
    );
    assert!(
        fill > 0.45,
        "the predicted chest fills only {:.1}% of its own projected rect \
         {chest_rect:?} ({predicted_in_rect} of {} px). Subject's non-sky bbox: {:?}",
        fill * 100.0,
        chest_rect.area(),
        bbox_of(&predicted_px, |px| is_non_sky(px, sky))
    );

    let (changed_rect, changed_count) = changed_bbox(&predicted_px, &unpredicted_px).expect(
        "the local prediction changed no pixel at all — the chain from \
         write_predicted_block to the screen is dead",
    );
    println!("changed bbox {changed_rect:?} ({changed_count} px)");
    let allowed = chest_rect.padded(2);
    assert!(
        allowed.x0 <= changed_rect.x0
            && allowed.y0 <= changed_rect.y0
            && changed_rect.x1 <= allowed.x1
            && changed_rect.y1 <= allowed.y1,
        "pixels changed outside the chest's projected rect: changed {changed_rect:?}, \
         allowed {allowed:?}"
    );

    // The two write paths must agree pixel for pixel: the prediction is the same
    // `set_block` + `sync_block_entity` pair the adapter's arm runs, and if the
    // frames differ then one of them is doing something extra to the record.
    assert!(
        changed_bbox(&predicted_px, &updated_px).is_none(),
        "the predicted chest is not pixel-identical to the same state delivered by \
         the BLOCK_UPDATE pair — the two write paths disagree. Differs at {:?}",
        changed_bbox(&predicted_px, &updated_px)
    );
}

/// A **refused** placement must lose its predicted block entity, not keep drawing
/// a chest in empty air.
///
/// That fix asks what happens when the server disagrees, and the answer is that no new
/// mechanism is needed: vanilla's server sends a `ClientboundBlockUpdatePacket` for
/// **both** the clicked position and the adjacent one after *every* `use_item_on`,
/// whatever it decided (`ServerGamePacketListenerImpl`'s own decompiled source) — so the
/// predicted cell is always overwritten within one round trip, and since that fix that
/// write calls `sync_block_entity`, which removes the record.
///
/// This gate is the difference between believing that and knowing it. The record
/// being removed here was created by the **prediction**, not by a packet, which is
/// the part that could conceivably have differed: a prediction that stashed its
/// record somewhere the correction does not reach would pass every test above and
/// leave a floating chest on every refused placement.
///
/// Modelled as the correction arriving for a placement that never happened
/// server-side, i.e. `air` at the predicted position. The final frame must be
/// pixel-identical to a world that never had a chest — not merely "mostly empty".
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_refused_placement_loses_the_predicted_block_entity() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();
    let eye = camera.position;

    let chest_state = predicted_placement_state(
        "minecraft:chest",
        &PlacedState {
            facing: Some(BlockFace::North),
            ..PlacedState::default()
        },
    )
    .expect("the shell must be willing to predict a chest");
    let air_state = first_state_named("minecraft:air");

    // The optimistic write.
    let (mut world, pos) = world_with_chunk();
    assert_eq!(
        write_predicted_block(&mut world, CHEST, chest_state),
        BlockEntitySync::Created
    );
    let predicted_spawns = gather(&world, pos, eye);
    assert_eq!(
        predicted_spawns.len(),
        1,
        "the prediction must be visible first, or the removal below proves nothing \
         — this is the premise of the whole test"
    );

    // The rect where a stale chest *would* paint, derived from that very spawn
    // through the same projection the draw uses — measured before the correction,
    // so it is the real geometry of the thing being removed rather than a
    // remembered rectangle.
    let models = BlockEntityModelSet::load();
    let instance = models
        .resolve_chest(&predicted_spawns[0])
        .expect("the predicted chest must resolve to a model in the corpus");
    let mesh = models.get(instance.model).expect("mesh");
    let chest_rect = posed_screen_rect(mesh, &instance.part_transforms, camera.view_projection());
    println!("chest rect (from real baked vertices): {chest_rect:?}");
    assert!(
        chest_rect.area() > 900,
        "the chest projects to only {} px — the camera, not the renderer, is wrong: \
         {chest_rect:?}",
        chest_rect.area()
    );

    // The server's correction, through the adapter's own `BLOCK_UPDATE` pair. The
    // block-entity type of `air` is `None`, which is what drives the removal; the
    // assertion names it so a census change that started handing back `Some` here
    // fails loudly instead of silently keeping the chest.
    assert_eq!(
        lodestone_data::block_states::StateId::new(air_state)
            .and_then(lodestone_data::block_entity_types::block_entity_type)
            .map(|kind| kind.raw()),
        None,
        "air must own no block entity"
    );
    assert_eq!(
        write_block(&mut world, CHEST, air_state, true),
        Some(BlockEntitySync::Removed),
        "the correction must drop the record the *prediction* created — if this is \
         `Kept` or `Absent` the two writers are not sharing one record"
    );
    let corrected_spawns = gather(&world, pos, eye);
    assert!(
        corrected_spawns.is_empty(),
        "a refused placement must stop being gathered: {corrected_spawns:?}"
    );

    // A world that never had a chest, for the pixel identity.
    let (never, never_pos) = world_with_chunk();
    let never_spawns = gather(&never, never_pos, eye);
    assert!(never_spawns.is_empty());

    let mut shoot = |spawns: Vec<ChestSpawn>| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        state.set_block_entity_source(move |_eye| spawns.clone());
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats)
    };
    let (predicted_px, predicted_stats) = shoot(predicted_spawns);
    let (corrected_px, corrected_stats) = shoot(corrected_spawns);
    let (never_px, never_stats) = shoot(never_spawns);

    // The premise in *pixels*, not just in spawns: the chest really did paint here
    // before the correction. Without this, "no chest is drawn afterwards" is
    // satisfiable by never having drawn one.
    assert_eq!(
        predicted_stats.block_entity_sheets_loaded,
        expected_sheets(),
        "expected every block-entity sheet from client.jar — without them \
         'no chest drew' is satisfied by a renderer that cannot draw one"
    );
    assert_eq!(predicted_stats.block_entities_drawn, 1);
    assert_eq!(corrected_stats.block_entities_drawn, 0);
    assert_eq!(never_stats.block_entities_drawn, 0);

    let sky = sky_bytes();
    let predicted_in_rect = non_sky_in(&predicted_px, chest_rect, sky);
    assert!(
        predicted_in_rect as f64 / chest_rect.area() as f64 > 0.45,
        "the predicted chest only paints {predicted_in_rect} px in {chest_rect:?}, so \
         its removal below would prove nothing. Non-sky bbox: {:?}",
        bbox_of(&predicted_px, |px| is_non_sky(px, sky))
    );
    let stale = non_sky_in(&corrected_px, chest_rect, sky);
    assert_eq!(
        stale, 0,
        "after the refusal the chest still paints {stale} px in {chest_rect:?} — a \
         stale predicted record is drawing a chest in empty air. Non-sky bbox of the \
         corrected frame: {:?}",
        bbox_of(&corrected_px, |px| is_non_sky(px, sky))
    );
    assert!(
        changed_bbox(&corrected_px, &never_px).is_none(),
        "the corrected frame must be pixel-identical to a world that never had a \
         chest; it differs at {:?}",
        changed_bbox(&corrected_px, &never_px)
    );
}

/// What else already paints here — **measured**, not assumed.
///
/// `CLAUDE.md` records a control that asserted a frame "clears uniformly" and
/// failed at 3.5% because of the unconditional first-person bare arm; that fix's
/// chest gate's own first failure bbox landed on that arm too. This locates it
/// and asserts it is disjoint from the rect the sibling gate measures, so that
/// gate's clean-control premise is a measurement rather than a hope.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_first_person_arm_is_disjoint_from_the_chest_rect() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();

    let state = RenderState::new(device, queue, format, W, H, None);
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
    let pixels = target.read_texels(device, queue);

    assert!(
        stats.first_person_arm_drawn,
        "this test's premise is that the arm paints unconditionally; if it does not, \
         the sibling gate's control is clean for a different reason than it claims"
    );
    assert_eq!(stats.block_entities_drawn, 0);

    let sky = sky_bytes();
    let (arm_rect, arm_count) = bbox_of(&pixels, |px| is_non_sky(px, sky))
        .expect("the arm draws, so a chest-free frame is not uniformly sky");
    println!("arm bbox {arm_rect:?} ({arm_count} px)");

    // The same rect the sibling gate derives, via the same gather.
    let (mut world, pos) = world_with_chunk();
    write_block(&mut world, CHEST, first_state_named("minecraft:chest"), true);
    let spawns = gather(&world, pos, camera.position);
    assert_eq!(spawns.len(), 1, "the gather must produce the chest under test");
    let models = BlockEntityModelSet::load();
    let instance = models.resolve_chest(&spawns[0]).expect("resolve");
    let mesh = models.get(instance.model).expect("mesh");
    let chest_rect = posed_screen_rect(mesh, &instance.part_transforms, camera.view_projection());

    assert!(
        !arm_rect.intersects(chest_rect),
        "the first-person arm ({arm_rect:?}) overlaps the chest's rect \
         ({chest_rect:?}). The sibling gate would then be measuring the arm, which is \
         exactly the false-control failure `CLAUDE.md` records. Move the chest or the \
         camera; do not relax the assertion."
    );
}
