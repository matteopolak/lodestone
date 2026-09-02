//! Advancements and statistics: long-lived progress tracking.
//!
//! Both are version-free canonical state. Advancements are a criteria graph the
//! server pushes incrementally; a statistics update is a batch of counters keyed
//! by a category and a value id.

use std::collections::HashMap;

use lodestone_model::event::ClientEvent;
use lodestone_model::Identifier;

/// Progress toward one advancement: which of its criteria have been obtained.
///
/// An advancement is *done* once every declared criterion is obtained. The
/// server sends the criteria list and pushes obtained criteria over time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdvancementProgress {
    /// Criterion name -> obtained (with an optional obtained-at timestamp in
    /// milliseconds; `Some` means obtained).
    criteria: HashMap<String, Option<i64>>,
}

impl AdvancementProgress {
    /// Builds progress from a list of criteria, all initially unobtained.
    #[must_use]
    pub fn from_criteria<I, S>(criteria: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            criteria: criteria.into_iter().map(|c| (c.into(), None)).collect(),
        }
    }

    /// Marks a criterion obtained at `timestamp` (ms). Adds it if unknown.
    pub fn obtain(&mut self, criterion: impl Into<String>, timestamp: i64) {
        self.criteria.insert(criterion.into(), Some(timestamp));
    }

    /// Clears a criterion's obtained state (revoke).
    pub fn revoke(&mut self, criterion: &str) {
        if let Some(slot) = self.criteria.get_mut(criterion) {
            *slot = None;
        }
    }

    /// Whether a criterion is obtained.
    #[must_use]
    pub fn is_obtained(&self, criterion: &str) -> bool {
        matches!(self.criteria.get(criterion), Some(Some(_)))
    }

    /// Whether every criterion is obtained (and there is at least one).
    #[must_use]
    pub fn is_done(&self) -> bool {
        !self.criteria.is_empty() && self.criteria.values().all(Option::is_some)
    }

    /// Fraction of criteria obtained, `0.0..=1.0`.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        if self.criteria.is_empty() {
            return 0.0;
        }
        let done = self.criteria.values().filter(|v| v.is_some()).count();
        done as f32 / self.criteria.len() as f32
    }
}

/// The client's advancement store, keyed by advancement id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Advancements {
    progress: HashMap<Identifier, AdvancementProgress>,
}

impl Advancements {
    /// A new empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces an advancement's progress.
    pub fn set(&mut self, id: Identifier, progress: AdvancementProgress) {
        self.progress.insert(id, progress);
    }

    /// Removes an advancement (a `remove` in the update packet).
    pub fn remove(&mut self, id: &Identifier) {
        self.progress.remove(id);
    }

    /// Looks up progress.
    #[must_use]
    pub fn get(&self, id: &Identifier) -> Option<&AdvancementProgress> {
        self.progress.get(id)
    }

    /// Mutable access, e.g. to obtain a criterion.
    pub fn get_mut(&mut self, id: &Identifier) -> Option<&mut AdvancementProgress> {
        self.progress.get_mut(id)
    }

    /// Number of tracked advancements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.progress.len()
    }

    /// Whether the store is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.progress.is_empty()
    }

    /// The ids of all completed advancements.
    pub fn completed(&self) -> impl Iterator<Item = &Identifier> {
        self.progress
            .iter()
            .filter(|(_, p)| p.is_done())
            .map(|(id, _)| id)
    }

    /// Applies one `update_advancements` payload, in vanilla order: reset (clear
    /// everything) → remove → add definitions → apply earned progress.
    ///
    /// A progress entry for an advancement that is neither being added now nor
    /// already known is dropped, mirroring the vanilla client (which logs an
    /// "unknown advancement" warning and skips it). Returns the ids of any such
    /// dropped entries so a caller can surface the same diagnostic.
    pub fn apply(&mut self, update: AdvancementsUpdate) -> Vec<Identifier> {
        if update.reset {
            self.progress.clear();
        }
        for id in &update.removed {
            self.progress.remove(id);
        }
        for added in update.added {
            self.progress
                .entry(added.id)
                .or_insert_with(|| AdvancementProgress::from_criteria(added.criteria));
        }
        let mut unknown = Vec::new();
        for (id, criteria) in update.progress {
            match self.progress.get_mut(&id) {
                Some(prog) => {
                    for (criterion, obtained) in criteria {
                        match obtained {
                            Some(ts) => prog.obtain(criterion, ts),
                            None => prog.revoke(&criterion),
                        }
                    }
                }
                None => unknown.push(id),
            }
        }
        unknown
    }
}

/// One advancement definition carried by an `update_advancements` payload: its
/// id and the names of the criteria that make it up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedAdvancement {
    /// The advancement id.
    pub id: Identifier,
    /// The criteria names comprising this advancement.
    pub criteria: Vec<String>,
}

/// Per-advancement earned progress carried by an update: the advancement id and,
/// for each criterion, its obtained-at timestamp in milliseconds (`None` means
/// unobtained).
pub type AdvancementProgressEntry = (Identifier, Vec<(String, Option<i64>)>);

/// A version-free representation of an `update_advancements` packet's payload.
///
/// A version adapter decodes the wire packet into this canonical shape and feeds
/// it to [`Advancements::apply`]; this crate never sees packet bytes. Field
/// semantics mirror the vanilla packet: `reset` replaces (rather than merges)
/// the whole store, `added` carries definitions, `removed` drops advancements,
/// and `progress` carries the full per-criterion earned map (a `Some(timestamp)`
/// marks a criterion obtained, `None` marks it unobtained).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdvancementsUpdate {
    /// Whether to clear the entire store before applying the rest.
    pub reset: bool,
    /// Advancement definitions being added or replaced.
    pub added: Vec<AddedAdvancement>,
    /// Advancement ids to remove.
    pub removed: Vec<Identifier>,
    /// Earned progress per advancement: `(id, [(criterion, obtained_ms)])`.
    pub progress: Vec<AdvancementProgressEntry>,
}

/// A statistic key: a category (e.g. `minecraft:mined`, `minecraft:custom`) and
/// a value id within it (e.g. `minecraft:stone`, `minecraft:jump`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StatKey {
    /// The statistic category / type registry id.
    pub category: Identifier,
    /// The value id within the category.
    pub value: Identifier,
}

impl StatKey {
    /// Builds a stat key.
    #[must_use]
    pub fn new(category: Identifier, value: Identifier) -> Self {
        Self { category, value }
    }
}

/// The client's statistics counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Statistics {
    values: HashMap<StatKey, i32>,
}

impl Statistics {
    /// A new empty statistics set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a statistic's value (statistics packets send absolute values).
    pub fn set(&mut self, key: StatKey, value: i32) {
        self.values.insert(key, value);
    }

    /// Adds `delta` to a statistic (local prediction between server syncs).
    pub fn increment(&mut self, key: StatKey, delta: i32) {
        *self.values.entry(key).or_default() += delta;
    }

    /// Reads a statistic (absent = `0`).
    #[must_use]
    pub fn get(&self, key: &StatKey) -> i32 {
        self.values.get(key).copied().unwrap_or(0)
    }

    /// Applies an `award_stats` batch: absolute counter values, one per changed
    /// statistic (vanilla sends totals, not deltas, so this sets rather than
    /// adds).
    pub fn apply<I>(&mut self, updates: I)
    where
        I: IntoIterator<Item = (StatKey, i32)>,
    {
        for (key, value) in updates {
            self.values.insert(key, value);
        }
    }

    /// Folds a [`ClientEvent::StatisticsAwarded`], returning whether the event
    /// belonged here.
    ///
    /// This is the `apply(&ClientEvent)` shape every other session store uses, so
    /// `lodestone_ecs::session` can register it like the rest; [`Self::apply`]
    /// stays as the version-free iterator form a test or a local prediction uses.
    ///
    /// An award whose `value` the adapter could not resolve is **skipped**, not
    /// stored under a placeholder key: [`StatKey`] has no "unknown value" and a
    /// synthetic one would collide across every unresolved statistic in the
    /// batch. The count is lost, which is the right trade — a wrong number on a
    /// statistics screen is worse than a zero.
    pub fn apply_event(&mut self, event: &ClientEvent) -> bool {
        let ClientEvent::StatisticsAwarded { stats } = event else {
            return false;
        };
        for award in stats {
            if let Some(value) = award.value.clone() {
                self.values
                    .insert(StatKey::new(award.stat_type.clone(), value), award.count);
            }
        }
        true
    }

    /// Number of non-default statistics tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether no statistics are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}
