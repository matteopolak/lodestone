use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use super::grants::{better_match, normalize_node, GrantMatch, GrantSet, PermissionLevel};

/// A named group: its own grants plus the groups it inherits from.
#[derive(Debug, Clone, Default)]
pub struct Group {
    pub grants: GrantSet,
    /// Groups this one inherits from. Resolved depth-first with a visited set,
    /// so a cycle terminates.
    pub parents: Vec<String>,
}

/// One subject's stored permission state: a command level plus grants plus
/// group memberships.
#[derive(Debug, Clone, Default)]
pub struct SubjectPermissions {
    pub level: PermissionLevel,
    pub grants: GrantSet,
    pub groups: Vec<String>,
}

/// Who we are asking about.
///
/// [`PermissionSubject::Console`] is vanilla's own all-permissions constant
/// — the server console and command blocks hold everything, and short-circuit
/// the whole resolution order rather than being modelled as an owner-level
/// player. Modelling it as a player would need a UUID nothing assigns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionSubject {
    Player(Uuid),
    Console,
}

/// The per-player and per-group grant store — a per-player and
/// per-group grant store, in-memory to start.
///
/// In-memory on purpose: persistence is the plugin's own job via
/// a persistent-data-container mechanism, not this module's.
#[derive(Debug, Default)]
pub struct PermissionStore {
    subjects: HashMap<Uuid, SubjectPermissions>,
    groups: HashMap<String, Group>,
    /// Groups every player is in without being named — LuckPerms' `default`
    /// group. Empty by default.
    default_groups: Vec<String>,
}

impl PermissionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subject(&self, player: Uuid) -> Option<&SubjectPermissions> {
        self.subjects.get(&player)
    }

    pub fn subject_mut(&mut self, player: Uuid) -> &mut SubjectPermissions {
        self.subjects.entry(player).or_default()
    }

    /// Set a player's command level. The **only** way a level is ever set
    /// today: no protocol family in this workspace carries an op level on the
    /// wire, so there is no ingest path to fold one in from.
    pub fn set_level(&mut self, player: Uuid, level: PermissionLevel) {
        self.subject_mut(player).level = level;
    }

    pub fn level(&self, player: Uuid) -> PermissionLevel {
        self.subjects
            .get(&player)
            .map(|s| s.level)
            .unwrap_or_default()
    }

    pub fn group(&self, name: &str) -> Option<&Group> {
        self.groups.get(&normalize_node(name))
    }

    pub fn group_mut(&mut self, name: &str) -> &mut Group {
        self.groups.entry(normalize_node(name)).or_default()
    }

    pub fn add_to_group(&mut self, player: Uuid, group: &str) {
        let group = normalize_node(group);
        let subject = self.subject_mut(player);
        if !subject.groups.contains(&group) {
            subject.groups.push(group);
        }
    }

    /// Groups every player belongs to implicitly (LuckPerms' `default`).
    pub fn add_default_group(&mut self, group: &str) {
        let group = normalize_node(group);
        if !self.default_groups.contains(&group) {
            self.default_groups.push(group);
        }
    }

    /// The best grant match for `node` across the subject's own grants and
    /// every group it inherits, transitively.
    ///
    /// Depth-first with a `visited` set: a group graph with a cycle
    /// (`staff` → `admin` → `staff`) terminates rather than recursing forever.
    /// `cyclic_group_inheritance_terminates` is the guard on that; the visited
    /// set is not an optimisation.
    pub(crate) fn collect_grants(&self, player: Uuid, node: &str) -> Option<GrantMatch> {
        let mut best: Option<GrantMatch> = None;
        let mut consider = |candidate: Option<GrantMatch>| {
            if let Some(candidate) = candidate {
                best = Some(match best {
                    None => candidate,
                    Some(current) => better_match(current, candidate),
                });
            }
        };

        let subject = self.subjects.get(&player);
        if let Some(subject) = subject {
            consider(subject.grants.best_match(node, true));
        }

        let mut visited: HashSet<String> = HashSet::new();
        let mut stack: Vec<String> = self
            .default_groups
            .iter()
            .cloned()
            .chain(
                subject
                    .map(|s| s.groups.clone())
                    .unwrap_or_default()
                    .into_iter(),
            )
            .collect();

        while let Some(name) = stack.pop() {
            if !visited.insert(name.clone()) {
                continue;
            }
            if let Some(group) = self.groups.get(&name) {
                consider(group.grants.best_match(node, false));
                stack.extend(group.parents.iter().map(|p| normalize_node(p)));
            }
        }

        best
    }
}
