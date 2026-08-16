//! Villager gossip (issue #244): the `GossipContainer`/`GossipType` port that
//! feeds reputation (issue #246, `crate::mobs::villager::reputation`).
//!
//! # What it is
//!
//! One villager's opinion ledger about every UUID it has an opinion of —
//! vanilla's `GossipContainer` (`.cache/mc/26.2/src/net/minecraft/world/entity/ai/gossip/GossipContainer.java`),
//! keyed on `GossipType` (`.../ai/gossip/GossipType.java`): five weighted,
//! decaying, capped counters (`major_negative`, `minor_negative`,
//! `minor_positive`, `major_positive`, `trading`) per `(target, type)` pair.
//!
//! # How it works
//!
//! [`GossipType::weight`]/[`GossipType::max`]/[`GossipType::decay_per_day`]/
//! [`GossipType::decay_per_transfer`] are the four constants vanilla's enum
//! constructor carries per variant, transcribed verbatim. [`GossipContainer`]
//! stores `HashMap<Uuid, HashMap<GossipType, i32>>` in place of vanilla's
//! `Map<UUID, EntityGossips>` (an `Object2IntOpenHashMap` per entity) — same
//! shape, ordinary collections. [`GossipContainer::add`] is
//! `GossipContainer.add`: merges via `mergeValuesForAddition` (sum, clamped
//! to the type's `max`, keeping the old value if the sum would drop it below
//! that cap from above — matches `sum > type.max ? Math.max(type.max,
//! oldValue) : sum` exactly) then
//! [`GossipContainer::discard_if_out_of_range`]'s two-sided clamp
//! (`makeSureValueIsntTooLowOrTooHigh`): a value at or above `2` after that
//! survives, otherwise the entry is dropped entirely — vanilla's
//! `DISCARD_THRESHOLD`. [`GossipContainer::decay`] is the daily tick
//! (`maybeDecayGossip`'s 24000-tick cadence lives on the caller, not here —
//! see this module's own doc for why); [`GossipContainer::transfer_from`] is
//! `transferFrom`/`selectGossipsForTransfer`'s weighted-without-replacement
//! draw, using the caller's RNG.
//!
//! # How to change it, and the gotchas
//!
//! - **`weighted_value` (`getReputation`'s inner sum) multiplies by
//!   `GossipType::weight`, which is signed** — `major_negative`'s weight is
//!   `-5`, so a stored *count* of `10` contributes `-50` to reputation, not
//!   `+10`. Reading a raw stored count as reputation directly (skipping the
//!   weight) silently inverts every negative gossip type.
//! - **`transfer_from`'s selection is weighted by `|weighted_value|`, not by
//!   the raw count** — a single `major_positive` entry (weight `5`) is five
//!   times as likely to be picked as a `trading` entry (weight `1`) with the
//!   same stored count, exactly as `GossipEntry.weightedValue().abs()` (via
//!   the cumulative-range construction, which sums `Math.abs(...)`) intends.
//! - **`decay_per_transfer` subtracts before the `>= 2` gate, not after
//!   `merge_values_for_transfer`'s `max`** — `transferFrom` computes
//!   `newGossip.value - newGossip.type.decayPerTransfer` and only merges if
//!   that lands at `2` or above; a value that decays below the threshold is
//!   dropped from the *transfer*, not merged as a zero.
//! - Adding a new gossip-driven consequence (a sixth `GossipType`, a new
//!   [`crate::mobs::villager::reputation::ReputationEventType`] arm) touches
//!   this file's enum and `reputation.rs`'s event-application match, not
//!   `MobSim` — this module has no dependency on `SimMob`/`MobSim` at all,
//!   matching `lodestone_server::villager_trade`'s own "pure logic, no wire,
//!   no `SimMob`" boundary.
//!
//! # What is not built, named rather than silent
//!
//! - **`GossipContainer::get_count_for_type`** (vanilla's own method of the
//!   same name) is not ported — nothing in this crate consumes it yet, and
//!   inventing an unused method here would be exactly the "computed for
//!   real, zero production readers" island this repo's own evidence
//!   standards call out.
//! - **The bare `remove(type)` sweep** (vanilla's `GossipContainer.remove(GossipType)`,
//!   which strips one type from every tracked entity at once) is not ported
//!   for the same reason — no caller in this crate needs it.
//! - **No on-disk persistence.** Matches every other villager-state gap this
//!   crate already discloses (`villager::WorkstationClaims`,
//!   `lodestone_server::villager_trade`'s offer state) — a restart loses
//!   every gossip entry.

use std::collections::HashMap;

use uuid::Uuid;

/// `GossipType` — vanilla's five gossip kinds, each carrying its own
/// weight/cap/decay constants (`.../ai/gossip/GossipType.java`'s enum
/// constructor arguments, transcribed verbatim).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GossipType {
    MajorNegative,
    MinorNegative,
    MinorPositive,
    MajorPositive,
    Trading,
}

impl GossipType {
    /// Every variant, in `GossipType.values()`'s own declaration order.
    pub const ALL: [GossipType; 5] = [
        GossipType::MajorNegative,
        GossipType::MinorNegative,
        GossipType::MinorPositive,
        GossipType::MajorPositive,
        GossipType::Trading,
    ];

    /// `GossipType.id` — the registry-style serialized name.
    #[must_use]
    pub fn path(self) -> &'static str {
        match self {
            Self::MajorNegative => "major_negative",
            Self::MinorNegative => "minor_negative",
            Self::MinorPositive => "minor_positive",
            Self::MajorPositive => "major_positive",
            Self::Trading => "trading",
        }
    }

    /// `GossipType.weight` — signed; every negative variant carries a
    /// negative weight, so a stored count multiplied by this is already the
    /// reputation contribution, not merely a magnitude.
    #[must_use]
    pub fn weight(self) -> i32 {
        match self {
            Self::MajorNegative => -5,
            Self::MinorNegative => -1,
            Self::MinorPositive => 1,
            Self::MajorPositive => 5,
            Self::Trading => 1,
        }
    }

    /// `GossipType.max` — the stored count's own ceiling (not a reputation
    /// ceiling; `weight` is applied on top).
    #[must_use]
    pub fn max(self) -> i32 {
        match self {
            Self::MajorNegative => 100,
            Self::MinorNegative => 200,
            Self::MinorPositive => 25,
            Self::MajorPositive => 20,
            Self::Trading => 25,
        }
    }

    /// `GossipType.decayPerDay`.
    #[must_use]
    pub fn decay_per_day(self) -> i32 {
        match self {
            Self::MajorNegative => 10,
            Self::MinorNegative => 20,
            Self::MinorPositive => 1,
            Self::MajorPositive => 0,
            Self::Trading => 2,
        }
    }

    /// `GossipType.decayPerTransfer`.
    #[must_use]
    pub fn decay_per_transfer(self) -> i32 {
        match self {
            Self::MajorNegative => 10,
            Self::MinorNegative => 20,
            Self::MinorPositive => 5,
            Self::MajorPositive => 20,
            Self::Trading => 20,
        }
    }
}

/// `GossipContainer.DISCARD_THRESHOLD` — a stored count below this is
/// dropped rather than kept at a near-zero value.
const DISCARD_THRESHOLD: i32 = 2;

/// One villager's gossip ledger — vanilla's `GossipContainer`. See this
/// module's own doc for the port's shape and gotchas.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GossipContainer {
    entries: HashMap<Uuid, HashMap<GossipType, i32>>,
}

impl GossipContainer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Read-only access to one target's raw stored counts, for a caller (or
    /// a test) that wants the un-weighted ledger rather than
    /// [`reputation`](Self::reputation)'s single summed number.
    #[must_use]
    pub fn entries_for(&self, target: Uuid) -> Option<&HashMap<GossipType, i32>> {
        self.entries.get(&target)
    }

    /// `GossipContainer.add`: merges `amount` into `(target, gtype)` via
    /// `mergeValuesForAddition`, then applies the two-sided clamp. A
    /// negative `amount` is how vanilla's own `remove(target, type, n)`
    /// (`add(target, type, -n)`) is expressed — no separate method needed.
    pub fn add(&mut self, target: Uuid, gtype: GossipType, amount: i32) {
        let bucket = self.entries.entry(target).or_default();
        let merged = match bucket.get(&gtype) {
            None => amount,
            Some(&old) => {
                let sum = old + amount;
                if sum > gtype.max() {
                    gtype.max().max(old)
                } else {
                    sum
                }
            }
        };
        if merged >= DISCARD_THRESHOLD {
            bucket.insert(gtype, merged.min(gtype.max()));
        } else {
            bucket.remove(&gtype);
        }
        if bucket.is_empty() {
            self.entries.remove(&target);
        }
    }

    /// `GossipContainer.remove(target, type)` — an outright drop, not a
    /// decayed subtraction.
    pub fn remove_type(&mut self, target: Uuid, gtype: GossipType) {
        if let Some(bucket) = self.entries.get_mut(&target) {
            bucket.remove(&gtype);
            if bucket.is_empty() {
                self.entries.remove(&target);
            }
        }
    }

    /// `EntityGossips.weightedValue` for one target, summed over every
    /// tracked [`GossipType`] — this **is** `getReputation(target, t ->
    /// true)`, vanilla's own predicate every real caller (`Villager.
    /// getPlayerReputation`) passes. A predicate-narrowed variant is not
    /// ported (see this module's doc); every consumer here wants the full
    /// sum.
    #[must_use]
    pub fn reputation(&self, target: Uuid) -> i32 {
        self.entries
            .get(&target)
            .map(|bucket| {
                bucket
                    .iter()
                    .map(|(gtype, &count)| count * gtype.weight())
                    .sum()
            })
            .unwrap_or(0)
    }

    /// `GossipContainer.decay`: every tracked entity's every entry drops by
    /// its type's `decayPerDay`, and an entry (or a now-empty entity) below
    /// [`DISCARD_THRESHOLD`] is dropped. The 24000-tick cadence
    /// (`maybeDecayGossip`) is the caller's job — this method is the
    /// unconditional daily step alone, so a caller can call it directly in a
    /// test without simulating a day.
    pub fn decay(&mut self) {
        self.entries.retain(|_, bucket| {
            bucket.retain(|gtype, count| {
                *count -= gtype.decay_per_day();
                *count >= DISCARD_THRESHOLD
            });
            !bucket.is_empty()
        });
    }

    /// Every unpacked `(target, type, value)` entry — vanilla's own
    /// `unpack()`, materialised as a `Vec` since this container is small
    /// (per-villager, a handful of tracked UUIDs) and both callers here
    /// (`transfer_from`'s selection, this method's own tests) want to
    /// iterate it more than once.
    fn unpack(&self) -> Vec<(Uuid, GossipType, i32)> {
        self.entries
            .iter()
            .flat_map(|(&target, bucket)| {
                bucket
                    .iter()
                    .map(move |(&gtype, &value)| (target, gtype, value))
            })
            .collect()
    }

    /// `GossipContainer.transferFrom`: pulls up to `max_count` gossip
    /// entries out of `source`, weighted by `|weightedValue|` (vanilla's
    /// cumulative-range-plus-binary-search draw, reduced here to the
    /// equivalent weighted linear scan — same distribution, no
    /// `fastutil`-shaped range array needed for a per-villager entry count
    /// this small), decays each by its `decay_per_transfer`, and merges any
    /// that still clear [`DISCARD_THRESHOLD`] into `self` by `max(old,
    /// new)` — vanilla's `mergeValuesForTransfer`.
    ///
    /// `next_int` is the caller's RNG draw (`RandomSource.nextInt(bound)`'s
    /// contract: a non-negative value in `[0, bound)`) — this module has no
    /// RNG type of its own, matching every other pure-logic module in this
    /// crate (`lodestone_server::villager_trade` takes no RNG at all because
    /// it needs none; this one takes a closure rather than depend on
    /// `crate::mob_spawn::SpawnRng` from a module that otherwise has zero
    /// dependency on anything outside `lodestone-data`/`uuid`).
    pub fn transfer_from(
        &mut self,
        source: &GossipContainer,
        mut next_int: impl FnMut(i32) -> i32,
        max_count: usize,
    ) {
        let entries = source.unpack();
        if entries.is_empty() {
            return;
        }
        let total_weight: i64 = entries
            .iter()
            .map(|&(_, gtype, value)| i64::from((value * gtype.weight()).abs()))
            .sum();
        if total_weight <= 0 {
            return;
        }
        let mut selected: std::collections::HashSet<(Uuid, GossipType)> =
            std::collections::HashSet::new();
        for _ in 0..max_count {
            let mut choice = i64::from(next_int(total_weight.min(i64::from(i32::MAX)) as i32));
            for &(target, gtype, value) in &entries {
                let weight = i64::from((value * gtype.weight()).abs());
                if choice < weight {
                    selected.insert((target, gtype));
                    break;
                }
                choice -= weight;
            }
        }
        for (target, gtype, value) in entries {
            if !selected.contains(&(target, gtype)) {
                continue;
            }
            let decayed = value - gtype.decay_per_transfer();
            if decayed < DISCARD_THRESHOLD {
                continue;
            }
            let bucket = self.entries.entry(target).or_default();
            let merged = match bucket.get(&gtype) {
                Some(&old) => old.max(decayed),
                None => decayed,
            };
            bucket.insert(gtype, merged);
        }
    }

    /// `GossipContainer.putAll` — merges every entry of `other` into `self`,
    /// overwriting on a `(target, type)` collision (vanilla's own
    /// `entries.putAll`, a plain map overwrite, not `add`'s clamp-and-merge).
    /// Used to seed a freshly cured villager's ledger from a zombie
    /// villager's saved gossip (issue #247).
    pub fn put_all(&mut self, other: &GossipContainer) {
        for (&target, bucket) in &other.entries {
            let dest = self.entries.entry(target).or_default();
            for (&gtype, &value) in bucket {
                dest.insert(gtype, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(byte: u8) -> Uuid {
        Uuid::from_bytes([byte; 16])
    }

    #[test]
    fn a_fresh_container_has_zero_reputation_for_anyone() {
        let container = GossipContainer::new();
        assert_eq!(container.reputation(uuid(1)), 0);
    }

    /// Predicted from the outside formula, not a round number: `10 *
    /// major_positive.weight()` (`5`) `= 50`.
    #[test]
    fn a_single_entry_contributes_its_count_times_weight() {
        let mut container = GossipContainer::new();
        container.add(uuid(1), GossipType::MajorPositive, 10);
        assert_eq!(container.reputation(uuid(1)), 50);
    }

    /// A negative-weight type must **lower** reputation, not raise it — the
    /// discriminating check this module's own doc warns a raw-count read
    /// would get backwards. `20 * major_negative.weight()` (`-5`) `= -100`.
    #[test]
    fn a_negative_gossip_type_lowers_reputation() {
        let mut container = GossipContainer::new();
        container.add(uuid(1), GossipType::MajorNegative, 20);
        assert_eq!(container.reputation(uuid(1)), -100);
    }

    /// Two different targets never share a ledger.
    #[test]
    fn reputation_is_per_target() {
        let mut container = GossipContainer::new();
        container.add(uuid(1), GossipType::MajorPositive, 10);
        container.add(uuid(2), GossipType::MajorNegative, 10);
        assert_eq!(container.reputation(uuid(1)), 50);
        assert_eq!(container.reputation(uuid(2)), -50);
    }

    /// `mergeValuesForAddition`: two additions that would sum past the
    /// type's `max` (`trading`'s `25`) clamp to `max`, not overflow it.
    /// `2 + 2 + ... ` (13 trades of `2` each `= 26`) must land at `25`, not
    /// `26`.
    #[test]
    fn repeated_additions_clamp_at_the_types_own_max() {
        let mut container = GossipContainer::new();
        for _ in 0..13 {
            container.add(uuid(1), GossipType::Trading, 2);
        }
        let stored = container
            .entries_for(uuid(1))
            .and_then(|bucket| bucket.get(&GossipType::Trading))
            .copied();
        assert_eq!(stored, Some(25), "trading's max is 25, not 26");
    }

    /// `remove_type` drops one `(target, type)` entry outright, leaves an
    /// unrelated type on the same target untouched, and drops the whole
    /// target entry once its last type is gone — the same "no leftover
    /// empty bucket" invariant `add`'s own discard path keeps.
    #[test]
    fn remove_type_drops_only_the_named_type() {
        let mut container = GossipContainer::new();
        container.add(uuid(1), GossipType::Trading, 10);
        container.add(uuid(1), GossipType::MajorPositive, 10);
        container.remove_type(uuid(1), GossipType::Trading);
        assert_eq!(
            container
                .entries_for(uuid(1))
                .and_then(|b| b.get(&GossipType::Trading)),
            None,
            "the named type must be gone"
        );
        assert_eq!(
            container
                .entries_for(uuid(1))
                .and_then(|b| b.get(&GossipType::MajorPositive))
                .copied(),
            Some(10),
            "an unrelated type on the same target must survive"
        );
        container.remove_type(uuid(1), GossipType::MajorPositive);
        assert_eq!(
            container.entries_for(uuid(1)),
            None,
            "removing the last type must drop the whole target entry, not leave an empty bucket"
        );
    }

    /// A negative addition that drops a stored value below
    /// `DISCARD_THRESHOLD` (`2`) removes the entry outright, and an entity
    /// with no remaining entries is dropped from the container too — not
    /// left behind as an empty bucket. `minor_positive` starts at `3`
    /// (`< max`), `-2` leaves `1`, below the threshold.
    #[test]
    fn a_value_decayed_below_the_discard_threshold_is_removed_entirely() {
        let mut container = GossipContainer::new();
        container.add(uuid(1), GossipType::MinorPositive, 3);
        container.add(uuid(1), GossipType::MinorPositive, -2);
        assert_eq!(
            container.entries_for(uuid(1)),
            None,
            "the only entry dropped below threshold, so the whole target entry must vanish"
        );
        assert!(container.is_empty());
    }

    /// `GossipContainer.decay`'s daily step: `major_negative`'s
    /// `decayPerDay` is `10`. A stored `15` decays to `5` (still `>= 2`,
    /// survives); a stored `11` decays to `1` (`< 2`, dropped).
    #[test]
    fn decay_subtracts_decay_per_day_and_drops_entries_below_threshold() {
        let mut container = GossipContainer::new();
        container.add(uuid(1), GossipType::MajorNegative, 15);
        container.add(uuid(2), GossipType::MajorNegative, 11);
        container.decay();
        assert_eq!(
            container
                .entries_for(uuid(1))
                .and_then(|b| b.get(&GossipType::MajorNegative))
                .copied(),
            Some(5)
        );
        assert_eq!(
            container.entries_for(uuid(2)),
            None,
            "11 - 10 = 1, below DISCARD_THRESHOLD, so the entry (and the now-empty \
             target bucket) must be gone"
        );
    }

    /// `major_positive`'s own `decayPerDay` is `0` — vanilla's own table
    /// (`GossipType.java`) says a major positive opinion never decays on
    /// its own. A neuter that substituted a nonzero decay for every type
    /// would pass every other decay test here and fail only this one.
    #[test]
    fn major_positive_gossip_never_decays() {
        let mut container = GossipContainer::new();
        container.add(uuid(1), GossipType::MajorPositive, 5);
        for _ in 0..1000 {
            container.decay();
        }
        assert_eq!(
            container
                .entries_for(uuid(1))
                .and_then(|b| b.get(&GossipType::MajorPositive))
                .copied(),
            Some(5),
            "major_positive.decay_per_day() must be 0"
        );
    }

    /// `transfer_from`'s decay-then-discard gate:
    /// `minor_positive.decay_per_transfer()` is `5`. A source entry of `6`
    /// decays to `1` on transfer — below threshold, so it must **not**
    /// appear on the receiver even though it was selected (deterministic
    /// selection here: only one entry exists, so every draw picks it).
    #[test]
    fn a_transferred_entry_that_decays_below_threshold_is_dropped() {
        let mut source = GossipContainer::new();
        source.add(uuid(9), GossipType::MinorPositive, 6);
        let mut dest = GossipContainer::new();
        dest.transfer_from(&source, |_| 0, 10);
        assert_eq!(
            dest.entries_for(uuid(9)),
            None,
            "6 - decay_per_transfer(5) = 1, below DISCARD_THRESHOLD"
        );
    }

    /// The surviving-transfer case, predicted exactly: a `major_positive`
    /// entry of `20` (at its own max) decays by `decay_per_transfer()`
    /// (`20`) to exactly `0`... which is itself below threshold. Use a
    /// `trading` entry instead: `25` (its max) minus `decay_per_transfer()`
    /// (`20`) `= 5`, which clears the `>= 2` gate and must land on the
    /// receiver at exactly `5`.
    #[test]
    fn a_transferred_entry_that_survives_decay_lands_at_the_decayed_value() {
        let mut source = GossipContainer::new();
        source.add(uuid(9), GossipType::Trading, 25);
        let mut dest = GossipContainer::new();
        dest.transfer_from(&source, |_| 0, 10);
        assert_eq!(
            dest.entries_for(uuid(9))
                .and_then(|b| b.get(&GossipType::Trading))
                .copied(),
            Some(5),
            "25 - decay_per_transfer(20) = 5"
        );
    }

    /// `mergeValuesForTransfer` is `max(old, new)`, not a sum — a receiver
    /// that already holds a higher value for `(target, type)` must keep it
    /// rather than being overwritten by a lower transferred one.
    #[test]
    fn transfer_merges_by_max_not_by_sum() {
        let mut source = GossipContainer::new();
        source.add(uuid(9), GossipType::Trading, 25); // decays to 5 on transfer
        let mut dest = GossipContainer::new();
        dest.add(uuid(9), GossipType::Trading, 20); // already higher than the transferred 5
        dest.transfer_from(&source, |_| 0, 10);
        assert_eq!(
            dest.entries_for(uuid(9))
                .and_then(|b| b.get(&GossipType::Trading))
                .copied(),
            Some(20),
            "max(20, 5) = 20, not 20 + 5 and not overwritten to 5"
        );
    }

    /// An empty source transfers nothing and must not panic on the
    /// division/modulo `total_weight` guards.
    #[test]
    fn transferring_from_an_empty_source_is_a_no_op() {
        let source = GossipContainer::new();
        let mut dest = GossipContainer::new();
        dest.transfer_from(&source, |_| 0, 10);
        assert!(dest.is_empty());
    }

    /// `put_all` overwrites rather than clamp-merging — the seed-a-cured-
    /// villager's-ledger use case (issue #247) needs the source value to
    /// land verbatim, not go through `add`'s max-cap arithmetic a second
    /// time.
    #[test]
    fn put_all_overwrites_the_destination_value_verbatim() {
        let mut source = GossipContainer::new();
        source.add(uuid(1), GossipType::MajorPositive, 20);
        let mut dest = GossipContainer::new();
        dest.add(uuid(1), GossipType::MajorPositive, 5);
        dest.put_all(&source);
        assert_eq!(
            dest.entries_for(uuid(1))
                .and_then(|b| b.get(&GossipType::MajorPositive))
                .copied(),
            Some(20)
        );
    }
}
