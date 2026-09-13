//! Deterministic redstone notification fan-out.

use crate::neighbor_update::{Notification, UPDATE_ORDER};
use lodestone_model::BlockPos;

/// The real default redstone-wire evaluator's power-strength update set, in
/// full: the set of centres to update is the wire's own position plus all six
/// of its neighbours, deduplicated; then every centre in that set gets a full
/// six-direction neighbor-update fan-out.
///
/// Seven *centres* — the wire's own position and each of its six neighbours —
/// each of which gets a full six-direction neighbor-update fan-out, so 42
/// notifications with duplicates among them. The real engine really does issue the
/// duplicates; the dedup only applies to the centres, not the notifications.
///
/// # Why the second layer is not a corner case
///
/// An earlier version handled centre 0 only and described the omission as
/// "a diagonal-over-conductor corner update". It is not: the geometry the
/// first layer alone cannot reach is the **standard torch-inverter** — dust
/// sitting on top of a block with a torch on that block's side. The torch is
/// diagonal to the dust, so it is a neighbour of a *neighbour* and only ever
/// appears in the second layer. Measured on live vanilla 26.2, that torch
/// inverts reliably; with the first layer alone we never notified it and it
/// stayed lit forever.
///
/// # Ordering
///
/// Vanilla iterates a `HashSet`, so its order is unspecified and cannot be
/// copied. This picks the one deterministic order available: centres in
/// `[pos] ++ UPDATE_ORDER`, and within each centre the six directions in
/// [`UPDATE_ORDER`]. Determinism is what this crate needs from it; no vanilla
/// behaviour can depend on an order vanilla itself does not guarantee.
pub(super) fn wire_update_centres(pos: BlockPos) -> Vec<BlockPos> {
    std::iter::once(pos).chain(UPDATE_ORDER.iter().map(|d| d.relative(pos))).collect()
}

/// [`wire_update_centres`] flattened into the notifications those seven
/// `updateNeighborsAt` calls issue, for use as a cascade return value. The two
/// are the same thing: the propagator resolves a returned notification and its
/// own cascade fully before moving to the next, which is exactly what
/// `updateNeighborsAt` does per centre.
pub(super) fn wire_update_fan_out(pos: BlockPos) -> Vec<Notification> {
    let mut out = Vec::with_capacity(UPDATE_ORDER.len() * (UPDATE_ORDER.len() + 1));
    for centre in wire_update_centres(pos) {
        for d in UPDATE_ORDER {
            out.push(Notification { pos: d.relative(centre), from: d });
        }
    }
    out
}
