//! Tests for chunk view tracking and delivery receipts.

use super::*;
use crate::server::tests::{NetherPacketAdmissionProtocol, RefusingChunkProtocol};
use crate::chunk::ChunkColumn;
use uuid::Uuid;

/// [`join_view_rings`]'s shape, at the three inputs that matter: the shell's
/// own radius, the degenerate 0, and a negative one.
///
/// Ring sizes are `1, 8, 16, …, 8r` and must sum to `(2r+1)²` with no
/// coordinate repeated — a ring walk that double-counted a corner or skipped
/// an edge would still be non-decreasing in distance, so the end-to-end gate
/// in `tests/serve_play.rs` checks set equality and this checks the counts.
#[test]
fn join_view_rings_partitions_the_square_exactly() {
    let rings = join_view_rings(9);
    assert_eq!(rings.len(), 10, "radius 9 has rings 0..=9");
    assert_eq!(rings[0], vec![(0, 0)], "ring 0 is the player's own column");
    for (r, ring) in rings.iter().enumerate() {
        let expected = if r == 0 { 1 } else { 8 * r };
        assert_eq!(ring.len(), expected, "ring {r} must hold {expected} columns");
        for &(dx, dz) in ring {
            assert_eq!(
                dx.abs().max(dz.abs()) as usize,
                r,
                "({dx}, {dz}) is not on ring {r}"
            );
        }
    }
    let flat: Vec<(i32, i32)> = rings.iter().flatten().copied().collect();
    let unique: HashSet<(i32, i32)> = flat.iter().copied().collect();
    assert_eq!(flat.len(), 361, "the rings must sum to (2*9+1)^2");
    assert_eq!(unique.len(), flat.len(), "no column may appear on two rings");
}

/// Radius 0 is one ring holding one column — the configuration several tests
/// in this crate join with.
#[test]
fn join_view_rings_at_radius_zero_is_a_single_column() {
    assert_eq!(join_view_rings(0), vec![vec![(0, 0)]]);
}

#[test]
fn delivered_columns_survive_only_while_they_remain_in_view() {
    let mut view = ViewTracker::new((0, 0), 1, 1);
    view.mark_delivered((0, 0));
    view.mark_delivered((1, 0));
    view.mark_delivered((9, 9));
    assert_eq!(view.delivered, HashSet::from([(0, 0), (1, 0)]));

    let _ = view.recenter(&RefusingChunkProtocol, 2, 0, None);
    assert_eq!(view.delivered, HashSet::from([(1, 0)]));
}

#[test]
fn served_full_stage_avoids_a_full_band_upgrade_with_shaped_control() {
    let coordinate = (2, 1);
    for (stage, expected_upgrades) in [
        (ChunkGenerationStage::Full, Vec::new()),
        (ChunkGenerationStage::Shaped, vec![coordinate]),
    ] {
        let mut view = ViewTracker::new_banded((0, 0), 3, 3, 0);
        let incarnation = view.incarnation(coordinate).unwrap();
        view.record_delivery(coordinate, incarnation, Some(stage));
        assert_eq!(view.columns[&coordinate].requested, ChunkGenerationStage::Shaped);
        assert_eq!(view.recenter(&RefusingChunkProtocol, 2, 1, None).upgrades, expected_upgrades);
    }
}

#[test]
fn shaped_receipt_keeps_a_higher_request_and_full_receipts_do_not_regress() {
    let coordinate = (2, 1);
    let mut view = ViewTracker::new_banded((0, 0), 3, 3, 0);
    let incarnation = view.incarnation(coordinate).unwrap();
    assert_eq!(view.recenter(&RefusingChunkProtocol, 2, 1, None).upgrades, vec![coordinate]);
    view.record_delivery(coordinate, incarnation, Some(ChunkGenerationStage::Shaped));
    assert_eq!(view.columns[&coordinate].requested, ChunkGenerationStage::Full);
    assert_eq!(view.columns[&coordinate].served, Some(ChunkGenerationStage::Shaped));
    assert_eq!(view.recenter(&RefusingChunkProtocol, 2, 2, None).upgrades, vec![(2, 2)]);
    assert!(view.recenter(&RefusingChunkProtocol, 2, 1, None).upgrades.is_empty());
    view.record_delivery(coordinate, incarnation, Some(ChunkGenerationStage::Full));
    view.record_delivery(coordinate, incarnation, Some(ChunkGenerationStage::Shaped));
    assert_eq!(view.columns[&coordinate].served, Some(ChunkGenerationStage::Full));
    assert!(!view.needs_delivery(coordinate, incarnation, Some(ChunkGenerationStage::Full)));
    assert!(!view.needs_delivery(coordinate, incarnation, Some(ChunkGenerationStage::Shaped)));
    assert!(view.needs_delivery(coordinate, incarnation, None));
}

#[test]
fn forgotten_and_reset_columns_reject_old_delivery_receipts() {
    let coordinate = (0, 0);
    let mut view = ViewTracker::new(coordinate, 0, 0);
    let old = view.incarnation(coordinate).unwrap();
    view.recenter(&RefusingChunkProtocol, 1, 0, None);
    view.recenter(&RefusingChunkProtocol, 0, 0, None);
    let reentered = view.incarnation(coordinate).unwrap();
    assert!(reentered.0 > old.0);
    view.record_delivery(coordinate, old, Some(ChunkGenerationStage::Full));
    assert!(view.delivered.is_empty());
    assert_eq!(view.columns[&coordinate].served, None);
    view.record_delivery(coordinate, reentered, Some(ChunkGenerationStage::Full));
    assert_eq!(view.delivered, HashSet::from([coordinate]));
    view.reset(coordinate);
    assert!(view.incarnation(coordinate).unwrap().0 > reentered.0);
    view.record_delivery(coordinate, reentered, Some(ChunkGenerationStage::Full));
    assert!(view.delivered.is_empty());
    assert_eq!(view.columns[&coordinate].served, None);
}

fn receipt_packet(payload: u8, stage: Option<ChunkGenerationStage>) -> EncodedColumn {
    EncodedColumn {
        directive: ServerDirective::Send { packet_id: 52, payload: vec![payload] },
        stage,
    }
}

struct ReceiptProtocol;

impl ServerProtocol for ReceiptProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        NetherPacketAdmissionProtocol.decode(state, packet_id, payload)
    }
    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }
    fn encode_chunk(&self, _cx: i32, _cz: i32, column: &ChunkColumn) -> ServerDirective {
        receipt_packet(u8::from(column.generation_stage() == ChunkGenerationStage::Full), None)
            .directive
    }
    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }
    fn retains_initial_column_light(&self) -> bool {
        true
    }
}

struct ReceiptSource { full: bool }

impl ChunkSource for ReceiptSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        unreachable!("receipt fixture requires no generation")
    }
    fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
        crate::chunk::air_state()
    }
    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        crate::chunk::DEFAULT_BIOME.to_owned()
    }
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
    fn settle_resident_column_lights_with_neighbours(
        &self,
        _cx: i32,
        _cz: i32,
        _fallback: &ChunkColumn,
        _neighbour_offsets: &[(i32, i32)],
        _resident_only: bool,
        _replace_existing: bool,
        _exclusive: bool,
        _compute: &mut dyn FnMut(
            &ChunkColumn, &[(i32, i32, &ChunkColumn)],
        ) -> Option<crate::chunk::ColumnLightSettlement>,
    ) -> Result<ChunkColumn, ColumnLightSettlementError> {
        if self.full { Ok(ChunkColumn::new(0, 16)) }
        else { Err(ColumnLightSettlementError::NoLight) }
    }
}

#[tokio::test]
async fn served_stage_follows_selected_centre_and_no_light_fallback() {
    let shaped = ChunkColumn::new(0, 16)
        .test_with_generation_stage(ChunkGenerationStage::Shaped);
    for (full, stage, payload) in [
        (true, ChunkGenerationStage::Full, vec![1]),
        (false, ChunkGenerationStage::Shaped, vec![0]),
    ] {
        let direct = encode_chunk_with_source_receipt(
            &ReceiptProtocol, &ReceiptSource { full }, 2, 1, &shaped,
        ).unwrap();
        assert_eq!(direct.stage, Some(stage));
        assert_eq!(direct.directive, ServerDirective::Send { packet_id: 52, payload });
        let owned = encode_column_owned(
            &ReceiptProtocol, Arc::new(ReceiptSource { full }), 2, 1, None,
            crate::join_scheduler::ColumnPayload::Column(shaped.clone()),
        ).await.unwrap();
        assert_eq!(owned.stage, Some(stage));
        assert_eq!(owned.directive, direct.directive);
    }
}

#[tokio::test]
async fn failed_chunk_write_does_not_promote_the_served_stage() {
    let coordinate = (2, 1);
    let mut view = ViewTracker::new_banded((0, 0), 3, 3, 0);
    let incarnation = view.incarnation(coordinate).unwrap();
    let (peer, endpoint) = lodestone_net::memory_pair();
    drop(peer);
    let error = send_encoded_column(
        &mut Connection::new(endpoint), &mut State::Play, &mut view,
        coordinate, incarnation, receipt_packet(1, Some(ChunkGenerationStage::Full)),
    ).await.unwrap_err();
    assert!(matches!(error, ServerError::Net(_)));
    assert_eq!(view.columns[&coordinate].served, None);
    assert!(view.delivered.is_empty());
    assert_eq!(view.recenter(&RefusingChunkProtocol, 2, 1, None).upgrades, vec![coordinate]);

    let (peer, endpoint) = lodestone_net::memory_pair();
    let mut connection = Connection::new(endpoint);
    assert!(send_encoded_column(
        &mut connection, &mut State::Play, &mut view,
        coordinate, incarnation, receipt_packet(2, Some(ChunkGenerationStage::Full)),
    ).await.unwrap());
    assert_eq!(view.columns[&coordinate].served, Some(ChunkGenerationStage::Full));
    assert_eq!(Connection::new(peer).read_packet().await.unwrap(), Some((52, vec![2])));
}

#[tokio::test]
async fn paused_encode_and_queued_batch_cannot_deliver_to_a_reentered_column() {
    let coordinate = (0, 0);
    let mut view = ViewTracker::new(coordinate, 0, 0);
    let old = view.incarnation(coordinate).unwrap();
    let (release, wait) = tokio::sync::oneshot::channel();
    let mut encodes = PendingJoinEncodes::new();
    encodes.push(true, Box::pin(async move {
        wait.await.unwrap();
        Ok((coordinate, (old, receipt_packet(1, Some(ChunkGenerationStage::Full)))))
    }));
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(encodes.poll_next(&mut context).is_pending());
    view.recenter(&RefusingChunkProtocol, 1, 0, None);
    view.recenter(&RefusingChunkProtocol, 0, 0, None);
    let current = view.incarnation(coordinate).unwrap();
    release.send(()).unwrap();
    let (_, (incarnation, encoded)) = std::future::poll_fn(|context| encodes.poll_next(context))
        .await.unwrap();
    let (peer, endpoint) = lodestone_net::memory_pair();
    let mut connection = Connection::new(endpoint);
    assert!(!send_encoded_column(
        &mut connection, &mut State::Play, &mut view, coordinate, incarnation, encoded,
    ).await.unwrap());
    send_pending_chunk_batch(
        &mut connection, &RefusingChunkProtocol, &mut State::Play, &mut view,
        PendingChunkBatch { columns: vec![
            (coordinate, old, receipt_packet(2, Some(ChunkGenerationStage::Full))),
            (coordinate, current, receipt_packet(3, Some(ChunkGenerationStage::Full))),
        ] },
    ).await.unwrap();
    assert_eq!(view.columns[&coordinate].served, Some(ChunkGenerationStage::Full));
    drop(connection);
    let mut peer = Connection::new(peer);
    assert_eq!(peer.read_packet().await.unwrap(), Some((40, Vec::new())));
    assert_eq!(peer.read_packet().await.unwrap(), Some((52, vec![3])));
    assert_eq!(peer.read_packet().await.unwrap(), Some((41, vec![1])));
    assert_eq!(peer.read_packet().await.unwrap(), None);
}

/// **The cross-arm invariant the off-centre join violated**, at a centre where
/// the two hypotheses actually differ.
///
/// Two independent constructions of one square: `join_view_rings` walks
/// Chebyshev rings and yields offsets, `ViewTracker::new` rasters a
/// `[-r, r]²` window around an absolute centre. The tracker's set is a *claim
/// about what the wire sent*, so if the two disagree the tracker suppresses
/// resends of columns the client never received. The expectation therefore
/// comes from neither implementation — it is the geometry both are supposed to
/// be describing.
///
/// `(25, -13)` deliberately: at a centre of `(0, 0)` the offset and absolute
/// readings coincide exactly, which is why every existing join gate — all of
/// which spawn at a position flooring to chunk `(0, 0)` — passed throughout.
#[test]
fn ring_offsets_plus_the_join_centre_are_the_square_the_view_tracker_seeds() {
    let radius = 9;
    let (cx, cz) = (25, -13);

    let emitted: HashSet<(i32, i32)> = join_view_rings(radius)
        .into_iter()
        .flatten()
        .map(|(dx, dz)| (cx + dx, cz + dz))
        .collect();
    let seeded = ViewTracker::new((cx, cz), radius, radius).loaded;

    assert_eq!(emitted.len(), 361, "radius 9 is 361 columns either way");
    assert_eq!(
        emitted, seeded,
        "the columns the join stream emits must be exactly the ones the tracker \
         records as sent; any difference is a column the client never gets and \
         never gets resent"
    );

    // The control, and it must fail the same assertion: the pre-fix code used
    // the raw offsets as absolute coordinates. Run and observed failing here
    // rather than described, because the *reason* this bug survived is that
    // the difference is invisible at the origin.
    let unshifted: HashSet<(i32, i32)> =
        join_view_rings(radius).into_iter().flatten().collect();
    assert_ne!(
        unshifted, seeded,
        "control failed: raw ring offsets must NOT equal the tracker's square at a \
         non-origin centre — if they do, this test cannot see the defect it exists for"
    );
    // And the reason the control has to be at a non-origin centre at all.
    assert_eq!(
        unshifted,
        ViewTracker::new((0, 0), radius, radius).loaded,
        "at the origin the two readings are identical, which is exactly why every \
         gate that spawns at chunk (0, 0) was blind to this"
    );
}

/// **A negative radius must yield no rings at all**, matching the raster walk
/// this replaced: `(-r..=r)` is an empty range for `r < 0`, so a negative
/// radius sent zero chunks. `view_radius.max(0)` would send one, and
/// `ViewTracker::new` would still record an empty loaded set for the same
/// input — the tracker and the wire disagreeing about a column the client
/// actually has. Nothing produces a negative radius today, which is why this
/// needs a test rather than a reading.
#[test]
fn join_view_rings_at_a_negative_radius_is_empty() {
    assert!(join_view_rings(-1).is_empty());
    assert!(join_view_rings(i32::MIN).is_empty());
}
