//! Server-authoritative advancement and statistic tracking (issue #338).
//!
//! Vanilla tracks these *on the server*: `net/minecraft/server/
//! PlayerAdvancements.java` holds per-player advancement progress,
//! `net/minecraft/stats/ServerStatsCounter.java` holds statistics, both persist
//! to disk, and the server streams them to the client with three packets:
//!
//! * `ClientboundUpdateAdvancementsPacket` — the advancement tree, per-player
//!   progress, and visibility. Vanilla sends it on join (the "first packet"
//!   carries the whole visible tree with `reset` true) and then whenever a
//!   dirty root or a changed progress needs broadcasting.
//!   [`ServerProtocol::encode_update_advancements`](crate::ServerProtocol::encode_update_advancements)
//!   is the seam for exactly that packet; this module builds the version-free
//!   payload.
//! * `ClientboundAwardStatsPacket` — a batch of `(stat, count)` pairs, sent in
//!   reply to the client's `ClientCommand(REQUEST_STATS)`.
//! * `ClientboundSelectAdvancementsTabPacket` — which tab the client should
//!   show (the client asks; vanilla answers with the same value).
//!
//! This crate is version-free, so the module owns the *data model and rules*,
//! never the wire bytes: advancement completion (`AdvancementRequirements`'s
//! AND-of-ORs test), progress dirtiness and the every-tick flush, the depth-2
//! visibility evaluator, and NBT persistence for the #437 hook. The protocol
//! crates turn the version-free payloads into packets.
//!
//! # Model vs vanilla
//!
//! * [`Advancement`] is a node in the tree: id, parent, requirement groups,
//!   and the `sendsTelemetryEvent` bit. Display info (title, icon, frame) is
//!   intentionally **absent** — it is pure presentation and this crate has no
//!   component model; a version crate's encoder may carry display fields from
//!   its own registry instead. See [`AdvancementManager::builtin`] for the
//!   builtin tree (real vanilla identifiers and criteria), which the data pack
//!   landing (part of this epic) will grow into a loader.
//! * [`AdvancementProgress`] is one advancement's per-criterion state: the
//!   criterion → obtained-epoch-millis map plus the requirement groups (so
//!   `is_done` is a property of the progress, exactly as vanilla's
//!   `AdvancementProgress.update(requirements)` keeps both).
//! * [`PlayerAdvancementState`] is the per-player bookkeeping vanilla's
//!   `PlayerAdvancements` does: progress keyed by advancement id, a dirty set,
//!   the visible-set cache, and the "is the first packet still pending" flag.
//! * [`PlayerStatistics`] is the per-player `Object2IntMap<Stat<?>>`.
//!
//! # Lifecycle
//!
//! 1. On join the server calls [`AdvancementManager::initial_update`]: it
//!    resets the client, sends the whole tree as "added", sends every
//!    advancement's current progress, and pre-computes visibility. Vanilla's
//!    `isFirstPacket` does the same.
//! 2. Gameplay code calls [`AdvancementManager::grant_criterion`] /
//!    [`AdvancementManager::revoke_criterion`] when a trigger fires. Each
//!    call returns a [`GrantOutcome`] so the caller can react to a *first*
//!    completion (vanilla's advancement "done" toast / root-unlocked logic).
//!
//!    **The trigger that actually fires today is
//!    [`AdvancementManager::on_inventory_changed`]**, vanilla's
//!    `minecraft:inventory_changed`, and it is deliberately the only one: every
//!    criterion in [`AdvancementManager::builtin`] that a player can reach uses
//!    that trigger with a single `items` predicate. `crate::server` drives it from
//!    the two places an item enters a player's inventory — the floor-pickup sweep
//!    and `/give`.
//!
//!    Putting the hook at the inventory seam rather than at each producer is not a
//!    shortcut, it is what the records say: `story/mine_stone` fires when you
//!    *obtain* cobblestone, from a dig or a chest or a command, so a block-break
//!    hook would grant it for a dig whose drop went nowhere and never grant it for
//!    the other routes. One hook reaches five of the seven builtin advancements.
//!
//!    **Statistics are driven from three sites**, each chosen because it is the
//!    place the count is naturally exactly-once: `minecraft:mined` in
//!    `crate::server`'s `destroy_block` (before the creative fork, so a creative
//!    break still counts), `minecraft:picked_up` in the pickup sweep (credited per
//!    item *banked*, not per entity seen), and the `minecraft:deaths` custom
//!    counter in `publish_health`, whose own guards already make crossing zero
//!    happen once per life.
//! 3. Every tick (vanilla: `ServerPlayer.tick()` calls
//!    `advancements.flushDirty(player, true)`) the server calls
//!    [`AdvancementManager::flush_dirty`]. Only when the first packet is still
//!    pending or a root/progress is dirty does it produce an
//!    [`AdvancementUpdate`] — this is the every-tick-no-op fast path.
//! 4. On save, [`AdvancementManager::save_advancements`] /
//!    [`AdvancementManager::save_statistics`] hand back NBT for the #437
//!    world-persistence hook; on load, the matching `load_*` restores it.
//!    The NBT mirrors vanilla's `PlayerAdvancements.asData()` (criteria
//!    map with obtained timestamps + `done` flag) rather than the JSON files
//!    on disk, because this crate persists NBT, not JSON.
//!
//! # Completion
//!
//! Vanilla's `AdvancementRequirements` is a list of groups; the advancement is
//! done when **every group** has **at least one** done criterion (AND of ORs).
//! An `allOf` advancement puts each criterion in its own group; an `anyOf` puts
//! all in one group. The empty group list (an advancement with no criteria)
//! is never done. [`AdvancementProgress::is_done`] implements this.
//!
//! # Visibility
//!
//! Vanilla's `AdvancementVisibilityEvaluator` walks the tree in order and only
//! *hides* a node when an ancestor at depth 2 is *done*; a node without a
//! display is hidden (this module's builtin tree has no display info, so
//! vanilla would hide those — see [`AdvancementManager::builtin`] for the
//! consequence). [`visible_ids`] reimplements the walk with the exact depth-2
//! window (`VISIBILITY_DEPTH = 2`): a node is shown if it or any descendant is
//! done, or if its parent or grandparent is done. A done ancestor further away
//! than a grandparent does not re-show the node.

use std::collections::{BTreeMap, BTreeSet};

use lodestone_core::Nbt;
use uuid::Uuid;

/// A single node in the advancement tree — id, parent, and completion shape.
///
/// Requirements are version-free stand-ins for vanilla's
/// `AdvancementRequirements` (a list of UTF string groups); the display info
/// (`DisplayInfo`) that determines icon/frame/toast is deliberately absent so
/// this crate never names a component model. `sendsTelemetryEvent` is carried
/// because it *is* on the wire (`Advancement.write`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advancement {
    /// The fully-qualified id, e.g. `minecraft:story/mine_stone`.
    pub id: String,
    /// Parent id, if any. A tree has exactly one node with `None` per root.
    pub parent: Option<String>,
    /// AND-of-ORs completion shape: done when every inner list has at least
    /// one granted criterion. Mirrors vanilla's `AdvancementRequirements`.
    pub requirements: Vec<Vec<String>>,
    /// The `sendsTelemetryEvent` bit carried on the wire.
    pub sends_telemetry_event: bool,
}

impl Advancement {
    /// A root advancement with the given completion shape.
    pub fn new(id: impl Into<String>, requirements: Vec<Vec<String>>, sends_telemetry_event: bool) -> Self {
        Self {
            id: id.into(),
            parent: None,
            requirements,
            sends_telemetry_event,
        }
    }

    /// Chain onto [`Advancement::new`] to set the parent.
    pub fn with_parent(mut self, parent: impl Into<String>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    /// Every criterion this advancement's requirements name, deduplicated.
    pub fn criterion_names(&self) -> BTreeSet<&str> {
        self.requirements.iter().flatten().map(String::as_str).collect()
    }
}

/// Why an advancement tree could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdvancementError {
    #[error("advancement `{0}` is defined twice")]
    DuplicateId(String),
    #[error("advancement `{0}` has no requirements (an empty advancement is never done)")]
    EmptyRequirements(String),
    #[error("advancement `{0}` has an empty requirement group")]
    EmptyRequirementGroup(String),
    #[error("advancement `{0}` names unknown parent `{1}`")]
    UnknownParent(String, String),
}

/// Progress on a single advancement: per-criterion obtained time plus the
/// requirements that decide completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvancementProgress {
    /// Criterion name → obtained epoch-millis (`None` = not yet obtained).
    criteria: BTreeMap<String, Option<i64>>,
    /// The AND-of-ORs completion shape this progress must satisfy.
    requirements: Vec<Vec<String>>,
}

impl AdvancementProgress {
    /// Build progress for every criterion a set of requirements names, all
    /// unobtained. Unknown criteria dropped on load are handled by [`grant`]
    /// no-oping for names outside `requirements`.
    pub fn new(requirements: Vec<Vec<String>>) -> Self {
        let mut criteria = BTreeMap::new();
        for name in requirements.iter().flatten() {
            criteria.insert(name.clone(), None);
        }
        Self {
            criteria,
            requirements,
        }
    }

    /// True when every requirement group has at least one obtained criterion.
    /// An advancement with no requirement groups is never done, matching
    /// vanilla's `AdvancementRequirements.test` (which returns `false` for an
    /// empty list).
    pub fn is_done(&self) -> bool {
        if self.requirements.is_empty() {
            return false;
        }
        self.requirements
            .iter()
            .all(|group| group.iter().any(|criterion| self.is_criterion_done(criterion)))
    }

    /// True once at least one criterion has been obtained.
    pub fn has_progress(&self) -> bool {
        self.criteria.values().any(Option::is_some)
    }

    /// Whether a single criterion is obtained.
    pub fn is_criterion_done(&self, criterion: &str) -> bool {
        self.criteria.get(criterion).is_some_and(Option::is_some)
    }

    /// Mark one criterion obtained. Returns `true` if it changed (vanilla's
    /// `award` returns a boolean "already-done" guard the caller ignores; we
    /// surface the *did-it-change* instead so a repeated trigger is not a
    /// dirty flush). A criterion not in the requirements is silently ignored,
    /// matching vanilla dropping unknown criteria in `update`.
    pub fn grant(&mut self, criterion: &str, obtained_millis: i64) -> bool {
        match self.criteria.get_mut(criterion) {
            Some(slot) if slot.is_none() => {
                *slot = Some(obtained_millis);
                true
            }
            _ => false,
        }
    }

    /// Revoke one criterion. Returns `true` if it changed.
    pub fn revoke(&mut self, criterion: &str) -> bool {
        match self.criteria.get_mut(criterion) {
            Some(slot) if slot.is_some() => {
                *slot = None;
                true
            }
            _ => false,
        }
    }
}

/// One advancement's progress as the client wants it: id plus the
/// criterion → obtained-epoch-millis pairs (`None` = not yet obtained).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvancementProgressUpdate {
    pub id: String,
    pub criteria: Vec<(String, Option<i64>)>,
}

impl AdvancementProgressUpdate {
    pub fn new(id: String, progress: &AdvancementProgress) -> Self {
        Self {
            id,
            criteria: progress
                .criteria
                .iter()
                .map(|(name, obtained)| (name.clone(), *obtained))
                .collect(),
        }
    }
}

/// The full payload of `ClientboundUpdateAdvancementsPacket` (26.2):
/// a reset flag, the added tree, removed ids, per-advancement progress, and
/// the show-advancements-screen flag. A version crate's
/// `encode_update_advancements` lowers exactly this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvancementUpdate {
    /// First packet (`true`): the client clears its entire advancement state
    /// and treats `added` as the whole tree. `false`: incremental.
    pub reset: bool,
    /// Advancements to add/update, carrying parent + requirements so the
    /// client can compute done-ness and layout.
    pub added: Vec<Advancement>,
    /// Advancement ids to remove (they became hidden).
    pub removed: Vec<String>,
    /// Per-advancement progress for every advancement that changed.
    pub progress: Vec<AdvancementProgressUpdate>,
    /// The show-advancements-in-chat flag (vanilla always sends `true`).
    pub show_advancements: bool,
}

/// The result of a single `grant_criterion` / `revoke_criterion` call.
///
/// The two booleans answer different questions: `changed` is "did the
/// criterion flip" (a re-trigger of an already-obtained criterion is not a
/// change and not a dirty flush); `completion_changed` is "did the
/// advancement's overall done state flip" (vanilla's
/// `rootAdvancements.get(predicate)` + toast logic fires only on a *newly*
/// completed root).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GrantOutcome {
    pub changed: bool,
    pub is_done: bool,
    pub completion_changed: bool,
}

/// A statistic key: a stat-type (mined/crafted/used/…) plus the value key the
/// type dispatches on (vanilla's `Stat.STREAM_CODEC` = `registry(STAT_TYPE)`
/// dispatch — an item/block/entity registry id for the typed kinds, a custom
/// stat id for `custom`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StatType {
    Mined,
    Crafted,
    Used,
    Broken,
    PickedUp,
    Dropped,
    Killed,
    KilledBy,
    Custom,
}

impl StatType {
    /// The real 26.2 stat-type registry key (from
    /// `generated/reports/registries.json`), which is what travels in the
    /// `ClientboundAwardStatsPacket` varint-encoded as the type's registry id.
    pub fn resource_key(self) -> &'static str {
        match self {
            Self::Mined => "minecraft:mined",
            Self::Crafted => "minecraft:crafted",
            Self::Used => "minecraft:used",
            Self::Broken => "minecraft:broken",
            Self::PickedUp => "minecraft:picked_up",
            Self::Dropped => "minecraft:dropped",
            Self::Killed => "minecraft:killed",
            Self::KilledBy => "minecraft:killed_by",
            Self::Custom => "minecraft:custom",
        }
    }

    /// Inverse of [`StatType::resource_key`]; `None` for an unknown key.
    pub fn from_resource_key(key: &str) -> Option<Self> {
        Some(match key {
            "minecraft:mined" => Self::Mined,
            "minecraft:crafted" => Self::Crafted,
            "minecraft:used" => Self::Used,
            "minecraft:broken" => Self::Broken,
            "minecraft:picked_up" => Self::PickedUp,
            "minecraft:dropped" => Self::Dropped,
            "minecraft:killed" => Self::Killed,
            "minecraft:killed_by" => Self::KilledBy,
            "minecraft:custom" => Self::Custom,
            _ => return None,
        })
    }
}

/// A fully-qualified statistic key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StatKey {
    pub kind: StatType,
    /// The value key: an item/block/entity registry id for the typed kinds
    /// (e.g. `minecraft:stone`), or a custom stat id (e.g. `play_one_minute`).
    pub value: String,
}

impl StatKey {
    pub fn new(kind: StatType, value: impl Into<String>) -> Self {
        Self {
            kind,
            value: value.into(),
        }
    }
}

/// A player's statistics counter, vanilla's `Object2IntMap<Stat<?>>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerStatistics {
    values: BTreeMap<StatKey, i32>,
}

impl PlayerStatistics {
    /// Bump a statistic and return the new value. Vanilla's
    /// `ServerStatsCounter.add` pushes a dirty marker and re-sends on the next
    /// `AwardStatsPacket`; here the caller owns when a snapshot is broadcast.
    pub fn increment(&mut self, key: &StatKey, by: i32) -> i32 {
        let entry = self.values.entry(key.clone()).or_insert(0);
        *entry += by;
        *entry
    }

    /// Set a statistic outright (restore-from-disk path).
    pub fn set(&mut self, key: StatKey, value: i32) {
        self.values.insert(key, value);
    }

    /// Current value (default 0 for a never-seen statistic).
    pub fn value(&self, key: &StatKey) -> i32 {
        self.values.get(key).copied().unwrap_or(0)
    }

    /// True when nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Every recorded `(key, value)` pair, ordered.
    pub fn iter(&self) -> impl Iterator<Item = (&StatKey, i32)> {
        self.values.iter().map(|(key, value)| (key, *value))
    }

    /// Persist as NBT, grouping by stat type exactly like vanilla's JSON
    /// `{"stats": {type: {value: count}}}` but in NBT compound form:
    /// `stats → {minecraft:mined → {minecraft:stone → 3}}`.
    pub fn to_nbt(&self) -> Nbt {
        let mut by_type: BTreeMap<&'static str, Vec<(String, Nbt)>> = BTreeMap::new();
        for (key, value) in &self.values {
            by_type
                .entry(key.kind.resource_key())
                .or_default()
                .push((key.value.clone(), Nbt::Int(*value)));
        }
        let stats = Nbt::Compound(
            by_type
                .into_iter()
                .map(|(kind, entries)| (kind.to_string(), Nbt::Compound(entries)))
                .collect(),
        );
        Nbt::Compound(vec![("stats".to_string(), stats)])
    }

    /// Restore from the [`PlayerStatistics::to_nbt`] shape, merging into the
    /// current values (call on a fresh, empty counter).
    pub fn from_nbt(&mut self, root: &Nbt) {
        let Nbt::Compound(fields) = root else {
            return;
        };
        for (name, value) in fields {
            if name != "stats" {
                continue;
            }
            let Nbt::Compound(by_type) = value else {
                continue;
            };
            for (kind_key, entries) in by_type {
                let Some(kind) = StatType::from_resource_key(kind_key) else {
                    continue;
                };
                let Nbt::Compound(values) = entries else {
                    continue;
                };
                for (value_key, count) in values {
                    if let Nbt::Int(count) = count {
                        self.set(StatKey::new(kind, value_key.clone()), *count);
                    }
                }
            }
        }
    }
}

/// Per-player advancement bookkeeping — vanilla's `PlayerAdvancements` in
/// miniature: progress keyed by id, the changed set, the visible-set cache,
/// and the pending-first-packet flag.
#[derive(Debug, Clone, Default)]
pub struct PlayerAdvancementState {
    /// Progress per advancement id. Created lazily as triggers fire and fully
    /// populated by `initial_update` / `from_nbt`.
    progress: BTreeMap<String, AdvancementProgress>,
    /// Advancement ids whose progress changed since the last flush.
    progress_changed: BTreeSet<String>,
    /// The currently-visible set, cached between flushes so `flush_dirty`
    /// only emits the *delta* (vanilla's `visibilityChanged` set).
    visible: BTreeSet<String>,
    /// True until the first `update_advancements` has been sent; the first
    /// flush then sends the whole tree with `reset` true.
    first_packet_pending: bool,
}

impl PlayerAdvancementState {
    fn is_done(&self, id: &str) -> bool {
        self.progress.get(id).is_some_and(AdvancementProgress::is_done)
    }

    /// Grant one criterion, recording the change for the next flush. Unknown
    /// advancements/requirements are no-ops. Callers pass the requirements so
    /// a lazily-created progress knows its completion shape.
    pub fn grant(
        &mut self,
        id: &str,
        requirements: &[Vec<String>],
        criterion: &str,
        obtained_millis: i64,
    ) -> GrantOutcome {
        let was_done = self.is_done(id);
        let progress = self
            .progress
            .entry(id.to_string())
            .or_insert_with(|| AdvancementProgress::new(requirements.to_vec()));
        let changed = progress.grant(criterion, obtained_millis);
        if changed {
            self.progress_changed.insert(id.to_string());
        }
        let is_done = progress.is_done();
        GrantOutcome {
            changed,
            is_done,
            completion_changed: was_done != is_done,
        }
    }

    /// Revoke one criterion, recording the change for the next flush.
    pub fn revoke(
        &mut self,
        id: &str,
        requirements: &[Vec<String>],
        criterion: &str,
    ) -> GrantOutcome {
        let was_done = self.is_done(id);
        let progress = self
            .progress
            .entry(id.to_string())
            .or_insert_with(|| AdvancementProgress::new(requirements.to_vec()));
        let changed = progress.revoke(criterion);
        if changed {
            self.progress_changed.insert(id.to_string());
        }
        let is_done = progress.is_done();
        GrantOutcome {
            changed,
            is_done,
            completion_changed: was_done != is_done,
        }
    }

    /// The current visible set for a fresh tree, computed with the depth-2
    /// evaluator over current completion.
    fn recompute_visible(&mut self, tree: &BTreeMap<String, Advancement>) {
        let is_done = |id: &str| self.is_done(id);
        let visible = visible_ids(tree, &is_done);
        self.visible = visible;
    }

    /// Persist as NBT: one compound keyed by advancement id, each value a
    /// `{criteria: {name: millis}, done: 1b}` compound — the NBT mirror of
    /// vanilla's `PlayerAdvancements.asData()` JSON (which skips advancements
    /// with no obtained criteria and stores obtained timestamps as strings;
    /// here timestamps are epoch-millis longs).
    pub fn to_nbt(&self) -> Nbt {
        let mut entries = Vec::new();
        for (id, progress) in &self.progress {
            if !progress.has_progress() {
                continue;
            }
            let criteria: Vec<(String, Nbt)> = progress
                .criteria
                .iter()
                .filter_map(|(name, obtained)| obtained.map(|millis| (name.clone(), Nbt::Long(millis))))
                .collect();
            entries.push((
                id.clone(),
                Nbt::Compound(vec![
                    ("criteria".to_string(), Nbt::Compound(criteria)),
                    ("done".to_string(), Nbt::Byte(progress.is_done() as i8)),
                ]),
            ));
        }
        Nbt::Compound(entries)
    }

    /// Restore persisted progress, merging into the current state. Only
    /// advancements present in `tree` are restored (vanilla warns and skips
    /// unknown ids — here a datapack change simply drops them); each restored
    /// advancement is marked dirty so the next flush re-broadcasts it.
    pub fn from_nbt(&mut self, tree: &BTreeMap<String, Advancement>, root: &Nbt) {
        let Nbt::Compound(fields) = root else {
            return;
        };
        for (id, value) in fields {
            let Some(adv) = tree.get(id) else {
                continue;
            };
            let mut progress = AdvancementProgress::new(adv.requirements.clone());
            if let Nbt::Compound(children) = value {
                for (child_name, child_value) in children {
                    if child_name != "criteria" {
                        continue;
                    }
                    if let Nbt::Compound(criteria) = child_value {
                        for (criterion, obtained) in criteria {
                            if let Nbt::Long(millis) = obtained {
                                progress.grant(criterion, *millis);
                            }
                        }
                    }
                }
            }
            if progress.has_progress() {
                self.progress.insert(id.clone(), progress);
                self.progress_changed.insert(id.clone());
            }
        }
    }
}

/// All per-player progression state held by the manager.
#[derive(Debug, Clone, Default)]
pub struct PlayerProgress {
    pub advancements: PlayerAdvancementState,
    pub statistics: PlayerStatistics,
}

/// The server-authoritative advancement/statistics store for every connected
/// player. Holds the advancement tree (shared) and per-uuid progress.
#[derive(Debug, Clone)]
pub struct AdvancementManager {
    tree: BTreeMap<String, Advancement>,
    players: BTreeMap<Uuid, PlayerProgress>,
}

impl AdvancementManager {
    /// Build a manager over a tree, validating it (unique ids, non-empty
    /// requirement lists, resolvable parents).
    pub fn new(tree: Vec<Advancement>) -> Result<Self, AdvancementError> {
        let mut map = BTreeMap::new();
        for adv in tree {
            if map.contains_key(&adv.id) {
                return Err(AdvancementError::DuplicateId(adv.id));
            }
            map.insert(adv.id.clone(), adv);
        }
        for adv in map.values() {
            if adv.requirements.is_empty() {
                return Err(AdvancementError::EmptyRequirements(adv.id.clone()));
            }
            if adv.requirements.iter().any(Vec::is_empty) {
                return Err(AdvancementError::EmptyRequirementGroup(adv.id.clone()));
            }
            if let Some(parent) = &adv.parent {
                if !map.contains_key(parent) {
                    return Err(AdvancementError::UnknownParent(adv.id.clone(), parent.clone()));
                }
            }
        }
        Ok(Self {
            tree: map,
            players: BTreeMap::new(),
        })
    }

    /// The builtin advancement tree: real vanilla identifiers, criteria and
    /// requirement shapes from 26.2's `data/minecraft/advancement/`.
    ///
    /// Note that every node here lacks display info, and vanilla's visibility
    /// evaluator *hides* display-less nodes — so of this tree only the roots
    /// and their done descendants would be visible to a real client until the
    /// data-pack loader (the next landing of this epic) supplies `DisplayInfo`.
    /// Completion logic, dirtiness and the flush protocol are all exercised
    /// regardless.
    ///
    /// The story chain mirrors vanilla's parent graph exactly:
    /// `story/root → story/mine_stone → story/upgrade_tools → story/smelt_iron →
    /// story/obtain_armor`. Requirement shapes are vanilla's verbatim:
    ///
    /// * `minecraft:story/root` — `crafting_table`, no parent.
    /// * `minecraft:story/mine_stone` — `get_stone` (punch stone).
    /// * `minecraft:story/upgrade_tools` — `stone_pickaxe`.
    /// * `minecraft:story/smelt_iron` — `iron`.
    /// * `minecraft:story/obtain_armor` — one **OR** group of
    ///   `iron_helmet`/`iron_chestplate`/`iron_leggings`/`iron_boots` (any one
    ///   piece completes it — vanilla's requirements are a single group, not
    ///   one group per piece).
    /// * `minecraft:nether/root` — `returned_safely`, no parent.
    /// * `minecraft:recipes/root` — `minecraft:impossible`, no parent, no display.
    pub fn builtin() -> Self {
        Self::new(vec![
            Advancement::new(
                "minecraft:story/root",
                vec![vec!["crafting_table".to_string()]],
                true,
            ),
            Advancement::new(
                "minecraft:story/mine_stone",
                vec![vec!["get_stone".to_string()]],
                true,
            )
            .with_parent("minecraft:story/root"),
            Advancement::new(
                "minecraft:story/upgrade_tools",
                vec![vec!["stone_pickaxe".to_string()]],
                true,
            )
            .with_parent("minecraft:story/mine_stone"),
            Advancement::new(
                "minecraft:story/smelt_iron",
                vec![vec!["iron".to_string()]],
                true,
            )
            .with_parent("minecraft:story/upgrade_tools"),
            Advancement::new(
                "minecraft:story/obtain_armor",
                vec![vec![
                    "iron_helmet".to_string(),
                    "iron_chestplate".to_string(),
                    "iron_leggings".to_string(),
                    "iron_boots".to_string(),
                ]],
                true,
            )
            .with_parent("minecraft:story/smelt_iron"),
            Advancement::new(
                "minecraft:nether/root",
                vec![vec!["returned_safely".to_string()]],
                true,
            ),
            Advancement::new(
                "minecraft:recipes/root",
                vec![vec!["minecraft:impossible".to_string()]],
                false,
            ),
        ])
        .expect("the builtin advancement tree is well-formed")
    }

    /// The item predicates every criterion in [`builtin`](Self::builtin) watches
    /// for, as `(item id, advancement id, criterion)`.
    ///
    /// **Read out of the 26.2 advancement records, not invented.** Every criterion
    /// in the builtin tree that a player can actually reach uses the
    /// `minecraft:inventory_changed` trigger with a single `items` predicate, so the
    /// whole trigger reduces to this table:
    ///
    /// | record | predicate | expanded to |
    /// |---|---|---|
    /// | `story/root` `crafting_table` | `minecraft:crafting_table` | one item |
    /// | `story/mine_stone` `get_stone` | `#minecraft:stone_tool_materials` | **three** items |
    /// | `story/upgrade_tools` `stone_pickaxe` | `minecraft:stone_pickaxe` | one item |
    /// | `story/smelt_iron` `iron` | `minecraft:iron_ingot` | one item |
    /// | `story/obtain_armor` × 4 | the four iron pieces | four items, one OR group |
    ///
    /// The `#minecraft:stone_tool_materials` tag is the row worth checking: it is
    /// **cobblestone, blackstone and cobbled deepslate** — *not* `minecraft:stone`.
    /// Mining a stone block drops cobblestone, which is why the advancement is
    /// reachable at all, and a table keyed on `minecraft:stone` would never fire
    /// for an ordinary dig.
    ///
    /// `nether/root`'s `returned_safely` and `recipes/root`'s
    /// `minecraft:impossible` are absent on purpose: the first is a dimension
    /// trigger this crate has no dimensions for, and the second is vanilla's own
    /// never-fires criterion.
    const INVENTORY_CHANGED_CRITERIA: &'static [(&'static str, &'static str, &'static str)] = &[
        ("minecraft:crafting_table", "minecraft:story/root", "crafting_table"),
        ("minecraft:cobblestone", "minecraft:story/mine_stone", "get_stone"),
        ("minecraft:blackstone", "minecraft:story/mine_stone", "get_stone"),
        ("minecraft:cobbled_deepslate", "minecraft:story/mine_stone", "get_stone"),
        ("minecraft:stone_pickaxe", "minecraft:story/upgrade_tools", "stone_pickaxe"),
        ("minecraft:iron_ingot", "minecraft:story/smelt_iron", "iron"),
        ("minecraft:iron_helmet", "minecraft:story/obtain_armor", "iron_helmet"),
        ("minecraft:iron_chestplate", "minecraft:story/obtain_armor", "iron_chestplate"),
        ("minecraft:iron_leggings", "minecraft:story/obtain_armor", "iron_leggings"),
        ("minecraft:iron_boots", "minecraft:story/obtain_armor", "iron_boots"),
    ];

    /// Vanilla's `minecraft:inventory_changed` trigger: `item` has just entered
    /// `player`'s inventory, so grant every criterion whose predicate it satisfies.
    ///
    /// # Why this is the hook and a block-break hook is not
    ///
    /// The obvious place to grant `story/mine_stone` is block-break finalisation,
    /// and it would be wrong. Vanilla's criterion is
    /// `minecraft:inventory_changed` on `#stone_tool_materials`, i.e. it fires when
    /// you **obtain** cobblestone — from a dig, from a chest, from a trade, from
    /// `/give`. A break hook would grant it for a dig whose drop went nowhere
    /// (a full inventory, `block_drops` off) and would never grant it for the other
    /// three routes.
    ///
    /// One hook here therefore lights up **five** of the seven builtin
    /// advancements, which is the whole reason to put it at the inventory seam
    /// rather than at each producer.
    ///
    /// Returns one [`GrantOutcome`] per criterion that changed, so a caller can
    /// tell whether anything is worth flushing. An item no criterion watches
    /// returns empty and touches nothing — which is the overwhelmingly common case,
    /// so the scan is a ten-entry slice comparison rather than a map.
    pub fn on_inventory_changed(
        &mut self,
        player: Uuid,
        item: &str,
        obtained_millis: i64,
    ) -> Vec<GrantOutcome> {
        let mut out = Vec::new();
        for &(watched, advancement, criterion) in Self::INVENTORY_CHANGED_CRITERIA {
            if watched != item {
                continue;
            }
            let outcome = self.grant_criterion(player, advancement, criterion, obtained_millis);
            if outcome.changed {
                out.push(outcome);
            }
        }
        out
    }

    /// The advancement tree, by id.
    pub fn tree(&self) -> &BTreeMap<String, Advancement> {
        &self.tree
    }

    /// Look up one advancement.
    pub fn advancement(&self, id: &str) -> Option<&Advancement> {
        self.tree.get(id)
    }

    /// Grant one criterion for a player. Unknown advancement or criterion is a
    /// no-op (`GrantOutcome::default()`).
    pub fn grant_criterion(
        &mut self,
        player: Uuid,
        advancement: &str,
        criterion: &str,
        obtained_millis: i64,
    ) -> GrantOutcome {
        let Some(adv) = self.tree.get(advancement) else {
            return GrantOutcome::default();
        };
        let requirements = adv.requirements.clone();
        let state = self.players.entry(player).or_default();
        state
            .advancements
            .grant(advancement, &requirements, criterion, obtained_millis)
    }

    /// Revoke one criterion for a player. Unknown advancement or criterion is
    /// a no-op.
    pub fn revoke_criterion(&mut self, player: Uuid, advancement: &str, criterion: &str) -> GrantOutcome {
        let Some(adv) = self.tree.get(advancement) else {
            return GrantOutcome::default();
        };
        let requirements = adv.requirements.clone();
        let state = self.players.entry(player).or_default();
        state.advancements.revoke(advancement, &requirements, criterion)
    }

    /// Whether a player has completed an advancement.
    pub fn is_done(&self, player: Uuid, advancement: &str) -> bool {
        self.players
            .get(&player)
            .is_some_and(|p| p.advancements.is_done(advancement))
    }

    /// Bump a statistic for a player, returning the new value. Records the
    /// player's state so a later [`AdvancementManager::stats_snapshot`] reply
    /// to `REQUEST_STATS` includes it.
    pub fn award_stat(&mut self, player: Uuid, key: StatKey, by: i32) -> i32 {
        let state = self.players.entry(player).or_default();
        state.statistics.increment(&key, by)
    }

    /// A player's current value for one statistic (default 0).
    pub fn stat_value(&self, player: Uuid, key: &StatKey) -> i32 {
        self.players
            .get(&player)
            .map(|p| p.statistics.value(key))
            .unwrap_or(0)
    }

    /// The `(key, value)` snapshot sent in reply to the client's
    /// `ClientCommand(REQUEST_STATS)` (vanilla's `ServerStatsCounter.sendStats`).
    pub fn stats_snapshot(&self, player: Uuid) -> Vec<(StatKey, i32)> {
        self.players
            .get(&player)
            .map(|p| p.statistics.iter().map(|(k, v)| (k.clone(), v)).collect())
            .unwrap_or_default()
    }

    /// The first-packet update: `reset` true, the whole tree as `added`, every
    /// advancement's current progress, and visibility pre-computed. Call once
    /// on join (vanilla's `isFirstPacket` path). Returns a packet to send.
    pub fn initial_update(&mut self, player: Uuid, show_advancements: bool) -> AdvancementUpdate {
        let tree_ids: Vec<String> = self.tree.keys().cloned().collect();
        let added: Vec<Advancement> = self.tree.values().cloned().collect();
        let state = self.players.entry(player).or_default();
        let mut progress = Vec::with_capacity(tree_ids.len());
        for id in &tree_ids {
            let requirements = self.tree[id].requirements.clone();
            let p = state
                .advancements
                .progress
                .entry(id.clone())
                .or_insert_with(|| AdvancementProgress::new(requirements));
            progress.push(AdvancementProgressUpdate::new(id.clone(), p));
        }
        state.advancements.visible = tree_ids.into_iter().collect();
        state.advancements.progress_changed.clear();
        state.advancements.first_packet_pending = false;
        AdvancementUpdate {
            reset: true,
            added,
            removed: Vec::new(),
            progress,
            show_advancements,
        }
    }

    /// The every-tick flush: produce an `update_advancements` payload only
    /// when the first packet is still pending or something changed. This is
    /// the no-op fast path vanilla's `PlayerAdvancements.flushDirty` takes
    /// every tick that nothing happened.
    pub fn flush_dirty(&mut self, player: Uuid, show_advancements: bool) -> Option<AdvancementUpdate> {
        let state = self.players.get_mut(&player)?;
        if !state.advancements.first_packet_pending && state.advancements.progress_changed.is_empty() {
            return None;
        }
        let reset = state.advancements.first_packet_pending;
        state.advancements.first_packet_pending = false;

        // Recompute visibility against current completion and emit the
        // delta against the previous visible set (vanilla's
        // `visibilityChanged` roots → added/removed).
        let old_visible = std::mem::take(&mut state.advancements.visible);
        state.advancements.recompute_visible(&self.tree);
        let target = std::mem::take(&mut state.advancements.visible);

        let mut added = Vec::new();
        for id in &target {
            if !old_visible.contains(id) {
                added.push(self.tree[id].clone());
            }
        }
        let mut removed = Vec::new();
        for id in &old_visible {
            if !target.contains(id) {
                removed.push(id.clone());
            }
        }
        state.advancements.visible = target;

        let changed: Vec<String> = state.advancements.progress_changed.iter().cloned().collect();
        state.advancements.progress_changed.clear();
        let mut progress = Vec::new();
        for id in changed {
            if state.advancements.visible.contains(&id) {
                if let Some(p) = state.advancements.progress.get(&id) {
                    progress.push(AdvancementProgressUpdate::new(id, p));
                }
            }
        }

        if reset || !added.is_empty() || !removed.is_empty() || !progress.is_empty() {
            Some(AdvancementUpdate {
                reset,
                added,
                removed,
                progress,
                show_advancements,
            })
        } else {
            None
        }
    }

    /// Per-player state (immutable), e.g. for the caller to compose persistence.
    pub fn player_progress(&self, player: Uuid) -> Option<&PlayerProgress> {
        self.players.get(&player)
    }

    /// Persist a player's advancements as NBT (the #437 hook).
    pub fn save_advancements(&self, player: Uuid) -> Nbt {
        self.players
            .get(&player)
            .map(|p| p.advancements.to_nbt())
            .unwrap_or_else(|| Nbt::Compound(Vec::new()))
    }

    /// Persist a player's statistics as NBT (the #437 hook).
    pub fn save_statistics(&self, player: Uuid) -> Nbt {
        self.players
            .get(&player)
            .map(|p| p.statistics.to_nbt())
            .unwrap_or_else(|| Nbt::Compound(Vec::new()))
    }

    /// Restore a player's advancements, merging into fresh progress built from
    /// the current tree. Advancements no longer in the tree are dropped; each
    /// restored one is re-broadcast on the next flush.
    pub fn load_advancements(&mut self, player: Uuid, root: &Nbt) {
        let state = self.players.entry(player).or_default();
        state.advancements.from_nbt(&self.tree, root);
    }

    /// Restore a player's statistics.
    pub fn load_statistics(&mut self, player: Uuid, root: &Nbt) {
        let state = self.players.entry(player).or_default();
        state.statistics.from_nbt(root);
    }
}

/// The depth-2 visibility walk from vanilla's
/// `AdvancementVisibilityEvaluator`: a node is visible when it or a descendant
/// is done, or when its parent or grandparent is done. A done node always
/// re-shows the chain up to it; a node whose done ancestor is further away than
/// a grandparent stays hidden.
pub fn visible_ids(
    tree: &BTreeMap<String, Advancement>,
    is_done: &dyn Fn(&str) -> bool,
) -> BTreeSet<String> {
    let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for adv in tree.values() {
        if let Some(parent) = adv.parent.as_deref() {
            children.entry(parent).or_default().push(&adv.id);
        }
    }
    let mut visible = BTreeSet::new();
    let mut ancestors = Vec::new();
    for adv in tree.values() {
        if adv.parent.is_none() {
            walk(&adv.id, &children, is_done, &mut ancestors, &mut visible);
        }
    }
    visible
}

fn walk(
    id: &str,
    children: &BTreeMap<&str, Vec<&str>>,
    is_done: &dyn Fn(&str) -> bool,
    ancestors: &mut Vec<bool>,
    visible: &mut BTreeSet<String>,
) -> bool {
    let self_done = is_done(id);
    ancestors.push(self_done);
    let len = ancestors.len();
    // Vanilla's `AdvancementVisibilityEvaluator.VISIBILITY_DEPTH = 2`: the
    // depth-2 window is the node's *own* rule plus its parent and grandparent
    // (the top-3 entries of the rule stack, `evaluateVisiblityForUnfinishedNode`
    // scanning `peek(0..=2)`). The node's own rule is `SHOW` exactly when it is
    // done — which `subtree_done` already covers below — so the *ancestor*
    // window is the parent and grandparent done-flags, `ancestors[len-2]` and
    // `ancestors[len-3]`. A great-grandparent's done state is out of window,
    // exactly as it is in vanilla.
    let ancestor_done = (len >= 2 && ancestors[len - 2])
        || (len >= 3 && ancestors[len - 3]);
    let mut descendant_done = false;
    if let Some(kids) = children.get(id) {
        for kid in kids {
            descendant_done |= walk(kid, children, is_done, ancestors, visible);
        }
    }
    ancestors.pop();
    let subtree_done = self_done || descendant_done;
    if subtree_done || ancestor_done {
        visible.insert(id.to_string());
    }
    subtree_done
}

#[cfg(test)]
mod tests {
    use super::*;

    fn story_tree() -> Vec<Advancement> {
        vec![
            Advancement::new("minecraft:story/root", vec![vec!["crafting_table".to_string()]], true),
            Advancement::new(
                "minecraft:story/mine_stone",
                vec![vec!["get_stone".to_string()]],
                true,
            )
            .with_parent("minecraft:story/root"),
            Advancement::new(
                "minecraft:story/obtain_armor",
                vec![vec![
                    "iron_helmet".to_string(),
                    "iron_chestplate".to_string(),
                    "iron_leggings".to_string(),
                    "iron_boots".to_string(),
                ]],
                true,
            )
            .with_parent("minecraft:story/root"),
        ]
    }

    #[test]
    fn builtin_tree_is_valid_with_expected_roots() {
        let manager = AdvancementManager::builtin();
        assert_eq!(manager.tree().len(), 7);
        // Exactly three roots.
        let roots = manager
            .tree()
            .values()
            .filter(|adv| adv.parent.is_none())
            .count();
        assert_eq!(roots, 3);
        // story/mine_stone's parent resolves and its single requirement is its
        // real criterion.
        let mine_stone = manager.advancement("minecraft:story/mine_stone").unwrap();
        assert_eq!(mine_stone.parent.as_deref(), Some("minecraft:story/root"));
        assert_eq!(mine_stone.criterion_names(), BTreeSet::from(["get_stone"]));
        // The story chain mirrors vanilla's parent graph:
        // root -> mine_stone -> upgrade_tools -> smelt_iron -> obtain_armor.
        let upgrade_tools = manager.advancement("minecraft:story/upgrade_tools").unwrap();
        assert_eq!(upgrade_tools.parent.as_deref(), Some("minecraft:story/mine_stone"));
        assert_eq!(upgrade_tools.criterion_names(), BTreeSet::from(["stone_pickaxe"]));
        let smelt_iron = manager.advancement("minecraft:story/smelt_iron").unwrap();
        assert_eq!(smelt_iron.parent.as_deref(), Some("minecraft:story/upgrade_tools"));
        assert_eq!(smelt_iron.criterion_names(), BTreeSet::from(["iron"]));
        // obtain_armor hangs off smelt_iron and is a single OR group of four.
        let armor = manager.advancement("minecraft:story/obtain_armor").unwrap();
        assert_eq!(armor.parent.as_deref(), Some("minecraft:story/smelt_iron"));
        assert_eq!(armor.requirements, vec![vec![
            "iron_helmet".to_string(),
            "iron_chestplate".to_string(),
            "iron_leggings".to_string(),
            "iron_boots".to_string(),
        ]]);
    }

    #[test]
    fn tree_validation_rejects_duplicates_and_bad_parents() {
        assert!(matches!(
            AdvancementManager::new(vec![
                Advancement::new("a", vec![vec!["x".to_string()]], false),
                Advancement::new("a", vec![vec!["y".to_string()]], false),
            ]),
            Err(AdvancementError::DuplicateId(_))
        ));
        assert!(matches!(
            AdvancementManager::new(vec![Advancement::new("a", vec![], false)]),
            Err(AdvancementError::EmptyRequirements(_))
        ));
        assert!(matches!(
            AdvancementManager::new(vec![
                Advancement::new("b", vec![vec!["x".to_string()]], false).with_parent("missing"),
            ]),
            Err(AdvancementError::UnknownParent(_, _))
        ));
        // A parent named before the child is fine.
        let manager = AdvancementManager::new(vec![
            Advancement::new("root", vec![vec!["x".to_string()]], false),
            Advancement::new("child", vec![vec!["y".to_string()]], false).with_parent("root"),
        ]);
        assert!(manager.is_ok());
    }

    #[test]
    fn initial_update_is_a_reset_full_tree() {
        let mut manager = AdvancementManager::new(story_tree()).unwrap();
        let player = Uuid::new_v4();
        let update = manager.initial_update(player, true);
        assert!(update.reset);
        assert!(update.removed.is_empty());
        assert!(update.show_advancements);
        assert_eq!(update.added.len(), 3);
        assert_eq!(update.progress.len(), 3);
        // Every tree id present in both added and progress.
        let added_ids: BTreeSet<_> = update.added.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(added_ids, BTreeSet::from([
            "minecraft:story/root",
            "minecraft:story/mine_stone",
            "minecraft:story/obtain_armor",
        ]));
        for p in &update.progress {
            // Nothing granted yet.
            assert!(p.criteria.iter().all(|(_, obtained)| obtained.is_none()));
        }
        // The first packet is consumed; an immediate flush is a silent no-op
        // (negative control for the absence assertion below).
        assert!(manager.flush_dirty(player, true).is_none());
    }

    #[test]
    fn granting_a_criterion_flushes_exactly_that_progress() {
        let mut manager = AdvancementManager::new(story_tree()).unwrap();
        let player = Uuid::new_v4();
        manager.initial_update(player, true);

        let outcome = manager.grant_criterion(player, "minecraft:story/mine_stone", "get_stone", 42);
        assert!(outcome.changed);
        assert!(outcome.is_done);
        assert!(outcome.completion_changed);

        let update = manager.flush_dirty(player, true).expect("dirty flush");
        assert!(!update.reset);
        assert!(update.added.is_empty());
        // The visibility recompute hides obtain_armor: it is not done, has no
        // done descendant, and its done relative (mine_stone) is a *sibling*,
        // not a parent/grandparent — so it is out of the depth-2 window.
        assert_eq!(update.removed, vec!["minecraft:story/obtain_armor".to_string()]);
        assert_eq!(update.progress.len(), 1);
        assert_eq!(update.progress[0].id, "minecraft:story/mine_stone");
        assert_eq!(update.progress[0].criteria, vec![("get_stone".to_string(), Some(42))]);

        // Flush drained the dirty set: a second flush is a no-op.
        assert!(manager.flush_dirty(player, true).is_none());
    }

    #[test]
    fn repeated_grants_are_not_changes() {
        let mut manager = AdvancementManager::new(story_tree()).unwrap();
        let player = Uuid::new_v4();
        manager.initial_update(player, true);
        let first = manager.grant_criterion(player, "minecraft:story/root", "crafting_table", 1);
        assert!(first.changed);
        let second = manager.grant_criterion(player, "minecraft:story/root", "crafting_table", 2);
        assert!(!second.changed);
        // The repeated grant did not dirty the flush again.
        let update = manager.flush_dirty(player, true).unwrap();
        assert_eq!(update.progress.len(), 1);
        assert_eq!(update.progress[0].criteria[0].1, Some(1));
        assert!(manager.flush_dirty(player, true).is_none());
    }

    #[test]
    fn obtain_armor_completes_on_any_single_piece() {
        // Vanilla's `story/obtain_armor` requirements are a *single* group of
        // the four pieces, i.e. an OR: any one piece completes it.
        let mut manager = AdvancementManager::new(story_tree()).unwrap();
        let player = Uuid::new_v4();
        manager.initial_update(player, true);
        let armor = "minecraft:story/obtain_armor";
        let outcome = manager.grant_criterion(player, armor, "iron_helmet", 1);
        assert!(outcome.changed);
        assert!(outcome.is_done, "a single piece completes a single-group requirement");
        assert!(outcome.completion_changed);
        // A second piece still flips the criterion, but the advancement was
        // already done, so the *completion* did not change.
        let second = manager.grant_criterion(player, armor, "iron_chestplate", 2);
        assert!(second.changed);
        assert!(second.is_done);
        assert!(!second.completion_changed);
        assert!(manager.is_done(player, armor));
    }

    #[test]
    fn all_of_advancement_requires_every_criterion() {
        // A genuinely AND-shaped advancement: each criterion in its own group,
        // so every one must be granted. Synthetic — the builtin tree mirrors
        // vanilla, whose all-of examples (e.g. `end/elytra`) are deeper than
        // this crate's builtin set.
        let mut manager = AdvancementManager::new(vec![
            Advancement::new(
                "minecraft:end/elytra",
                vec![
                    vec!["a".to_string()],
                    vec!["b".to_string()],
                    vec!["c".to_string()],
                ],
                true,
            ),
        ])
        .unwrap();
        let player = Uuid::new_v4();
        manager.initial_update(player, true);
        let id = "minecraft:end/elytra";
        for (i, criterion) in ["a", "b"].iter().enumerate() {
            let outcome = manager.grant_criterion(player, id, criterion, 1 + i as i64);
            assert!(outcome.changed);
            assert!(!outcome.is_done, "{criterion} alone must not complete an AND advancement");
            assert!(!outcome.completion_changed);
        }
        // The last criterion is the one that flips completion.
        let outcome = manager.grant_criterion(player, id, "c", 3);
        assert!(outcome.is_done);
        assert!(outcome.completion_changed);
        assert!(manager.is_done(player, id));
    }

    #[test]
    fn or_group_completes_on_any_single_criterion() {
        let mut manager = AdvancementManager::new(vec![
            Advancement::new("minecraft:nether/root", vec![vec!["x".to_string(), "y".to_string()]], true),
        ])
        .unwrap();
        let player = Uuid::new_v4();
        manager.initial_update(player, true);
        assert!(manager.grant_criterion(player, "minecraft:nether/root", "y", 1).is_done);
        assert!(manager.is_done(player, "minecraft:nether/root"));
    }

    #[test]
    fn revoke_removes_completion_and_dirtiness() {
        let mut manager = AdvancementManager::new(story_tree()).unwrap();
        let player = Uuid::new_v4();
        manager.initial_update(player, true);
        let id = "minecraft:story/mine_stone";
        assert!(manager.grant_criterion(player, id, "get_stone", 1).is_done);
        assert!(manager.is_done(player, id));
        let outcome = manager.revoke_criterion(player, id, "get_stone");
        assert!(outcome.changed);
        assert!(outcome.completion_changed);
        assert!(!manager.is_done(player, id));
        // Revoking an already-revoked criterion is not a change.
        assert!(!manager.revoke_criterion(player, id, "get_stone").changed);
    }

    #[test]
    fn unknown_advancement_or_criterion_is_a_noop() {
        let mut manager = AdvancementManager::new(story_tree()).unwrap();
        let player = Uuid::new_v4();
        manager.initial_update(player, true);
        assert_eq!(
            manager.grant_criterion(player, "minecraft:not_real", "x", 1),
            GrantOutcome::default()
        );
        assert_eq!(
            manager.grant_criterion(player, "minecraft:story/root", "not_a_criterion", 1),
            GrantOutcome::default()
        );
        assert!(!manager.is_done(player, "minecraft:story/root"));
        // Nothing was dirtied by the no-ops.
        assert!(manager.flush_dirty(player, true).is_none());
    }

    #[test]
    fn completion_of_a_child_reveals_its_chain() {
        let tree = story_tree();
        let done = |id: &str| id == "minecraft:story/mine_stone";
        let visible = visible_ids(&BTreeMap::from_iter(tree.into_iter().map(|a| (a.id.clone(), a))), &done);
        // mine_stone is done -> itself shown; root is an ancestor -> shown;
        // obtain_armor is unrelated and not done -> hidden.
        assert!(visible.contains("minecraft:story/mine_stone"));
        assert!(visible.contains("minecraft:story/root"));
        assert!(!visible.contains("minecraft:story/obtain_armor"));
    }

    #[test]
    fn nbt_round_trip_preserves_advancements_and_stats() {
        let mut manager = AdvancementManager::new(story_tree()).unwrap();
        let player = Uuid::new_v4();
        manager.initial_update(player, true);
        manager.grant_criterion(player, "minecraft:story/mine_stone", "get_stone", 7);
        manager.award_stat(
            player,
            StatKey::new(StatType::Mined, "minecraft:stone"),
            3,
        );
        manager.award_stat(
            player,
            StatKey::new(StatType::Custom, "minecraft:leave_game"),
            1,
        );

        let adv_nbt = manager.save_advancements(player);
        let stats_nbt = manager.save_statistics(player);

        // Persisted progress carries the obtained timestamp and done flag.
        let Nbt::Compound(adv_fields) = &adv_nbt else { panic!("advancement nbt is a compound") };
        let (_, entry) = adv_fields
            .iter()
            .find(|(id, _)| id == "minecraft:story/mine_stone")
            .expect("mine_stone persisted");
        let Nbt::Compound(entry_fields) = entry else { panic!("entry is a compound") };
        let Nbt::Compound(criteria) = &entry_fields
            .iter()
            .find(|(k, _)| k == "criteria")
            .expect("criteria map present")
            .1
        else { panic!("criteria is a compound") };
        assert_eq!(criteria, &vec![("get_stone".to_string(), Nbt::Long(7))]);
        let done = entry_fields
            .iter()
            .find(|(k, _)| k == "done")
            .map(|(_, v)| match v {
                Nbt::Byte(b) => *b,
                _ => panic!("done is a byte"),
            })
            .expect("done flag present");
        assert_eq!(done, 1);

        // Restore into a fresh manager over the same tree.
        let mut fresh = AdvancementManager::new(story_tree()).unwrap();
        let player2 = Uuid::new_v4();
        fresh.load_advancements(player2, &adv_nbt);
        fresh.load_statistics(player2, &stats_nbt);
        assert!(fresh.is_done(player2, "minecraft:story/mine_stone"));
        assert_eq!(
            fresh.stats_snapshot(player2),
            vec![
                (StatKey::new(StatType::Mined, "minecraft:stone"), 3),
                // StatKey orders by StatType first; Mined < Custom.
                (StatKey::new(StatType::Custom, "minecraft:leave_game"), 1),
            ]
        );
        // Restored progress is re-broadcast on the next flush.
        let update = fresh.flush_dirty(player2, true).expect("restored progress is dirty");
        assert!(!update.reset);
        assert!(update.progress.iter().any(|p| p.id == "minecraft:story/mine_stone"));
    }

    #[test]
    fn nbt_load_drops_advancements_gone_from_the_tree() {
        let player = Uuid::new_v4();
        // A save mentioning an id the current tree no longer has.
        let stale = Nbt::Compound(vec![(
            "minecraft:story/removed".to_string(),
            Nbt::Compound(vec![(
                "criteria".to_string(),
                Nbt::Compound(vec![("x".to_string(), Nbt::Long(1))]),
            )]),
        )]);
        let mut fresh = AdvancementManager::new(story_tree()).unwrap();
        fresh.load_advancements(player, &stale);
        assert!(!fresh.is_done(player, "minecraft:story/removed"));
        assert!(!fresh.advancement("minecraft:story/removed").is_some());
        // And a criterion not in the tree's requirements is dropped too.
        let extra = Nbt::Compound(vec![(
            "minecraft:story/root".to_string(),
            Nbt::Compound(vec![(
                "criteria".to_string(),
                Nbt::Compound(vec![("not_a_criterion".to_string(), Nbt::Long(1))]),
            )]),
        )]);
        fresh.load_advancements(player, &extra);
        assert!(!fresh.is_done(player, "minecraft:story/root"));
    }

    #[test]
    fn stats_increment_and_snapshot_in_order() {
        let mut manager = AdvancementManager::new(story_tree()).unwrap();
        let player = Uuid::new_v4();
        let stone = StatKey::new(StatType::Mined, "minecraft:stone");
        assert_eq!(manager.award_stat(player, stone.clone(), 3), 3);
        assert_eq!(manager.award_stat(player, stone.clone(), 2), 5);
        assert_eq!(manager.stat_value(player, &stone), 5);
        assert_eq!(
            manager.stats_snapshot(player),
            vec![(StatKey::new(StatType::Mined, "minecraft:stone"), 5)]
        );
        // A player with no state replies empty (not an error).
        assert!(manager.stats_snapshot(Uuid::new_v4()).is_empty());
    }

    #[test]
    fn stat_type_resource_keys_round_trip() {
        for kind in [
            StatType::Mined,
            StatType::Crafted,
            StatType::Used,
            StatType::Broken,
            StatType::PickedUp,
            StatType::Dropped,
            StatType::Killed,
            StatType::KilledBy,
            StatType::Custom,
        ] {
            assert_eq!(StatType::from_resource_key(kind.resource_key()), Some(kind));
        }
        assert_eq!(StatType::from_resource_key("minecraft:nope"), None);
    }
    // ---------------------------------------------------------------------
    // The `minecraft:inventory_changed` trigger — the hook that turned
    // `grant_criterion` from a tested function with zero callers into something
    // a player can actually reach.
    // ---------------------------------------------------------------------

    /// **The cheapest end-to-end claim, and the one the epic asked for first**:
    /// obtaining cobblestone completes `minecraft:story/mine_stone`, and the
    /// completion shows up in the packet a flush would send.
    ///
    /// The item is **cobblestone, not stone**, and that is the whole point of the
    /// table being read out of the records: `story/mine_stone`'s criterion is
    /// `minecraft:inventory_changed` on `#minecraft:stone_tool_materials`, which the
    /// 26.2 tag expands to cobblestone / blackstone / cobbled deepslate. A hook
    /// keyed on `minecraft:stone` — the obvious guess, and what a "block-break
    /// criterion" would have used — never fires for an ordinary dig.
    #[test]
    fn obtaining_cobblestone_completes_mine_stone_and_reaches_the_flush() {
        let mut manager = AdvancementManager::builtin();
        let player = Uuid::from_u128(9);
        // Clear the first-packet flag, so what follows is a real incremental flush
        // rather than the join snapshot.
        let _ = manager.initial_update(player, true);
        let _ = manager.flush_dirty(player, true);

        assert!(
            !manager.is_done(player, "minecraft:story/mine_stone"),
            "control: nothing is granted before the trigger fires"
        );

        let outcomes = manager.on_inventory_changed(player, "minecraft:cobblestone", 1_000);
        assert_eq!(outcomes.len(), 1, "exactly one criterion watches cobblestone");
        assert!(outcomes[0].is_done, "get_stone is the only requirement, so it completes");
        assert!(manager.is_done(player, "minecraft:story/mine_stone"));

        // And it reaches the wire. `flush_dirty` returning `None` here would be the
        // island: granted server-side, invisible to the client forever.
        let update = manager
            .flush_dirty(player, true)
            .expect("a granted criterion must produce an update to send");
        let entry = update
            .progress
            .iter()
            .find(|p| p.id == "minecraft:story/mine_stone")
            .expect("the flushed update must name the completed advancement");
        assert!(
            entry
                .criteria
                .iter()
                .any(|(name, obtained)| name == "get_stone" && *obtained == Some(1_000)),
            "the flushed criterion must carry the timestamp it was granted with, or              the client cannot order the toast: {:?}",
            entry.criteria
        );
    }

    /// The control for the table itself: an item **no** criterion watches must grant
    /// nothing and leave the player's state untouched. Without this, a trigger that
    /// granted `story/root` for everything would pass the gate above.
    ///
    /// `minecraft:stone` is deliberately the negative case — it is the item a
    /// careless implementation would have keyed `get_stone` on.
    #[test]
    fn an_unwatched_item_grants_nothing() {
        let mut manager = AdvancementManager::builtin();
        let player = Uuid::from_u128(9);
        for item in ["minecraft:stone", "minecraft:dirt", "minecraft:diamond"] {
            assert!(
                manager.on_inventory_changed(player, item, 1).is_empty(),
                "{item} is not in any builtin criterion's predicate"
            );
        }
        assert!(!manager.is_done(player, "minecraft:story/root"));
        assert!(!manager.is_done(player, "minecraft:story/mine_stone"));
    }

    /// Every item in the transcribed table really does grant its own criterion, and
    /// the five reachable builtin advancements really are reachable — so the table
    /// is exercised as a whole rather than only through its cobblestone row.
    ///
    /// The count is asserted, because a table row that silently named a
    /// non-existent advancement or criterion would make `grant_criterion` a no-op
    /// and nothing else here would notice.
    #[test]
    fn every_table_row_grants_a_real_criterion() {
        for &(item, advancement, criterion) in AdvancementManager::INVENTORY_CHANGED_CRITERIA {
            let mut manager = AdvancementManager::builtin();
            let player = Uuid::from_u128(1);
            let outcomes = manager.on_inventory_changed(player, item, 1);
            assert_eq!(
                outcomes.len(),
                1,
                "{item} must grant exactly {advancement}/{criterion}"
            );
            assert!(
                manager.is_done(player, advancement),
                "{advancement} has one requirement group containing {criterion}, so \
                 {item} alone completes it"
            );
        }
        // Five distinct advancements are reachable through the table; the other two
        // builtin nodes (`nether/root`, `recipes/root`) are deliberately absent.
        let reachable: std::collections::BTreeSet<&str> =
            AdvancementManager::INVENTORY_CHANGED_CRITERIA
                .iter()
                .map(|&(_, advancement, _)| advancement)
                .collect();
        assert_eq!(
            reachable.len(),
            5,
            "one hook lights up five of the seven builtin advancements: {reachable:?}"
        );
    }

    /// `story/obtain_armor` is a single **OR** group of four pieces, so any one
    /// piece completes it — and the second piece must report no further change.
    ///
    /// This is the row where the requirement *shape* matters: modelled as four
    /// separate groups, one helmet would leave the advancement incomplete.
    #[test]
    fn any_single_iron_piece_completes_obtain_armor() {
        let mut manager = AdvancementManager::builtin();
        let player = Uuid::from_u128(4);
        let first = manager.on_inventory_changed(player, "minecraft:iron_boots", 1);
        assert_eq!(first.len(), 1);
        assert!(first[0].is_done, "one piece of the OR group is enough");

        let second = manager.on_inventory_changed(player, "minecraft:iron_helmet", 2);
        assert_eq!(second.len(), 1, "the criterion itself is newly granted");
        assert!(
            !second[0].completion_changed,
            "the advancement was already done, so completion must not change again"
        );
    }
}
