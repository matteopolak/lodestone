//! The bounded terrain admission contract for an initial join.
//!
//! A client can observe a resident centre column before that column has been
//! meshed, or before the neighbouring columns needed by border lighting are
//! available. The server therefore has a stricter milestone than "the first
//! chunk packet exists": the centre and its immediate eight-neighbour
//! footprint must be admitted, and the centre's initial light must be settled,
//! before the centre is allowed to release the loading phase. The rest of the
//! view remains a streaming job.

use std::collections::HashSet;

/// The smallest complete playable neighbourhood around the player's column.
///
/// Radius one is a 3×3 footprint: the centre plus all eight columns a player
/// can step into immediately. It is intentionally independent of render
/// distance; increasing the view must not turn the loading barrier into a full
/// view barrier.
pub const INITIAL_PLAYABLE_RADIUS: i32 = 1;

/// The terrain admitted before an initial join may release its centre column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitialJoinReadiness {
    centre: (i32, i32),
    view_radius: i32,
}

impl InitialJoinReadiness {
    /// Creates a readiness contract centred on the player's initial chunk.
    #[must_use]
    pub const fn new(centre: (i32, i32), view_radius: i32) -> Self {
        Self {
            centre,
            view_radius,
        }
    }

    /// The chunk the client uses as its own initial column.
    #[must_use]
    pub const fn centre(self) -> (i32, i32) {
        self.centre
    }

    /// The bounded number of columns in the initial playable footprint.
    #[must_use]
    pub const fn required_count(self) -> usize {
        if self.view_radius <= 0 { 1 } else { 9 }
    }

    /// Returns the centre and immediate neighbours in deterministic ring order.
    ///
    /// The centre is first because this is the set definition, not the wire
    /// release order. Callers that emit a centre packet as the readiness signal
    /// should use [`Self::release_order`] or otherwise hold that packet until
    /// every member of this set has been admitted.
    #[must_use]
    pub fn required_columns(self) -> Vec<(i32, i32)> {
        let radius = if self.view_radius <= 0 {
            0
        } else {
            INITIAL_PLAYABLE_RADIUS
        };
        let mut columns = Vec::with_capacity(self.required_count());
        columns.push(self.centre);
        if radius == 0 {
            return columns;
        }
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if (dx != 0 || dz != 0) && dx.abs().max(dz.abs()) <= radius {
                    columns.push((self.centre.0 + dx, self.centre.1 + dz));
                }
            }
        }
        columns
    }

    /// Returns the minimal release order for a centre-residency loading gate.
    ///
    /// The eight neighbours precede the centre for a radius-one view. For a
    /// radius-zero view the centre is necessarily the only column. Columns
    /// beyond this list remain in the ordinary outward streaming queue.
    #[must_use]
    pub fn release_order(self) -> Vec<(i32, i32)> {
        let mut columns = self.required_columns();
        if columns.len() > 1 {
            let centre = columns
                .iter()
                .position(|&column| column == self.centre)
                .expect("required footprint always contains its centre");
            let centre = columns.remove(centre);
            columns.push(centre);
        }
        columns
    }

    /// Whether every column in the bounded footprint is already admitted.
    #[must_use]
    pub fn area_is_admitted<I>(self, admitted: I) -> bool
    where
        I: IntoIterator<Item = (i32, i32)>,
    {
        let required: HashSet<_> = self.required_columns().into_iter().collect();
        admitted
            .into_iter()
            .collect::<HashSet<_>>()
            .is_superset(&required)
    }

    /// Checks the server-side order at which a centre packet is released.
    ///
    /// This is deliberately an admission-order predicate, not a client packet
    /// decoder. It lets a server test prove that a centre packet cannot be the
    /// readiness signal while the required footprint is still missing.
    #[must_use]
    pub fn centre_is_released_after<I>(self, admitted_order: I) -> bool
    where
        I: IntoIterator<Item = (i32, i32)>,
    {
        let required: HashSet<_> = self.required_columns().into_iter().collect();
        let mut admitted = HashSet::with_capacity(required.len());
        for column in admitted_order {
            admitted.insert(column);
            if column == self.centre {
                return admitted.is_superset(&required);
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radius_zero_requires_only_the_centre() {
        let readiness = InitialJoinReadiness::new((17, -4), 0);

        assert_eq!(readiness.required_count(), 1);
        assert_eq!(readiness.required_columns(), vec![(17, -4)]);
        assert_eq!(readiness.release_order(), vec![(17, -4)]);
        assert!(readiness.centre_is_released_after([(17, -4)]));
    }

    #[test]
    fn radius_one_is_a_three_by_three_footprint_with_centre_last_on_release() {
        let readiness = InitialJoinReadiness::new((-11, 23), 9);
        let required = readiness.required_columns();
        let release = readiness.release_order();

        assert_eq!(required.len(), 9);
        assert_eq!(required.iter().copied().collect::<HashSet<_>>().len(), 9);
        assert_eq!(release.len(), 9);
        assert_eq!(release.last().copied(), Some((-11, 23)));
        assert!(release[..8].iter().all(|&(cx, cz)| {
            (cx + 11).abs().max((cz - 23).abs()) == INITIAL_PLAYABLE_RADIUS
        }));
        assert!(readiness.centre_is_released_after(release));
    }

    #[test]
    fn centre_first_is_a_negative_control_for_the_admission_detector() {
        let readiness = InitialJoinReadiness::new((0, 0), 1);
        let old_order = readiness.required_columns();

        assert_eq!(old_order.first().copied(), Some((0, 0)));
        assert!(!readiness.centre_is_released_after(old_order));
        assert!(readiness.centre_is_released_after(readiness.release_order()));
    }

    #[test]
    fn the_initial_barrier_does_not_expand_with_the_view() {
        let readiness = InitialJoinReadiness::new((0, 0), 32);
        let admitted = readiness.required_columns();

        assert_eq!(readiness.required_count(), 9);
        assert_eq!(admitted.len(), 9);
        assert!(!admitted.contains(&(2, 0)));
        assert!(readiness.area_is_admitted(admitted));
    }
}
