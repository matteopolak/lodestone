use std::collections::BTreeMap;

/// Vanilla's five command levels, transliterated from 26.2's
/// own permission-level enum — same variants, same
/// ids, same equal-or-higher-than comparison.
///
/// A description of "four op levels (2-4 for the built-in
/// commands, plus the `op`/non-op boolean)" misses one. There are **five** (0 through 4),
/// and `ALL`=0 is the level a non-op holds rather than the absence of a level;
/// `ops.json`'s `level` field is exactly this id
/// (vanilla's own op-list-entry type reads it through its own level-by-id lookup).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum PermissionLevel {
    /// 0 — every player, op or not. Vanilla's `ALL`.
    #[default]
    All,
    /// 1 — vanilla's `MODERATORS`. The lowest level `ops.json` records, so
    /// this is where [`PermissionLevel::is_op`] starts returning `true`.
    Moderators,
    /// 2 — vanilla's `GAMEMASTERS`. Most cheat-adjacent vanilla commands, and
    /// the level vanilla requires for entity selectors.
    Gamemasters,
    /// 3 — vanilla's `ADMINS`.
    Admins,
    /// 4 — vanilla's `OWNERS`.
    Owners,
}

impl PermissionLevel {
    /// The numeric id, matching `ops.json`'s `level` field and vanilla's own
    /// level-id accessor.
    pub fn id(self) -> u8 {
        match self {
            Self::All => 0,
            Self::Moderators => 1,
            Self::Gamemasters => 2,
            Self::Admins => 3,
            Self::Owners => 4,
        }
    }

    /// Vanilla's own level-name accessor.
    pub fn serialized_name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Moderators => "moderators",
            Self::Gamemasters => "gamemasters",
            Self::Admins => "admins",
            Self::Owners => "owners",
        }
    }

    /// Vanilla's own level-by-id lookup, **including its clamping**: the
    /// upstream is a continuous id-to-enum map with a clamping out-of-bounds
    /// strategy, so an
    /// out-of-range id saturates rather than wrapping or failing. An
    /// `ops.json` hand-edited to `"level": 9` really does mean `OWNERS` in
    /// vanilla, and a negative id means `ALL`.
    pub fn by_id(id: i32) -> Self {
        match id {
            i32::MIN..=0 => Self::All,
            1 => Self::Moderators,
            2 => Self::Gamemasters,
            3 => Self::Admins,
            _ => Self::Owners,
        }
    }

    /// Vanilla's own equal-or-higher-than comparison.
    pub fn is_equal_or_higher_than(self, other: Self) -> bool {
        self.id() >= other.id()
    }

    /// Whether this level counts as "op" for Bukkit's own op-status check, which is what
    /// [`PermissionDefault::Op`] is evaluated against.
    ///
    /// Defined as `>= MODERATORS`, because `ops.json` has no entry for a
    /// non-op at all — the lowest level it can record is 1, so being in the op
    /// list and being at least `MODERATORS` are the same condition. A player
    /// at [`PermissionLevel::All`] is not in the file and is not an op.
    pub fn is_op(self) -> bool {
        self.is_equal_or_higher_than(Self::Moderators)
    }
}

/// Vanilla's own permission sum type — a sum of a
/// namespaced atom and a command-level requirement.
///
/// Both variants exist because vanilla really does mix them in one interface:
/// its own built-in gamemaster-commands permission is a command-level
/// requirement, while
/// its own send-chat-commands permission is a plain named atom (`"chat/send_commands"`). A
/// plugin's own nodes are always [`Permission::Atom`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Permission {
    /// A named node. Vanilla spells these with `/` inside an `Identifier`
    /// (`minecraft:chat/send_commands`); Bukkit and every plugin spell them
    /// with `.` (`myplugin.admin.reload`). Both are just strings here and both
    /// are normalised the same way — only `.` is treated as the wildcard
    /// segment separator, so a vanilla-style `chat/send_messages` node works
    /// but cannot be wildcard-matched segment-wise, which is correct: vanilla
    /// has no wildcards.
    Atom(String),
    /// "is this subject at command level `N` or higher" — vanilla's
    /// `HasCommandLevel`, "is this player at least op level
    /// N" in vanilla's own spelling.
    HasCommandLevel(PermissionLevel),
}

impl Permission {
    /// A plugin node from a dotted string.
    pub fn atom(node: impl Into<String>) -> Self {
        Self::Atom(node.into())
    }
}

/// Bukkit's own permission-default enum — what a *declared*
/// node resolves to when no grant matched.
///
/// All four values, with Bukkit's exact op-status resolution table. A
/// three-value description (`true`/`false`/`op`) misses one; [`PermissionDefault::NotOp`]
/// is the fourth and is not decorative — it is how a plugin makes a node that
/// staff specifically *lack* (a "show the newbie hints" toggle, say).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionDefault {
    /// Everyone holds it.
    True,
    /// Nobody holds it without an explicit grant.
    False,
    /// Ops hold it. Bukkit's global fallback for an undeclared node, hence
    /// this being the `Default`.
    #[default]
    Op,
    /// Non-ops hold it, ops do not.
    NotOp,
}

impl PermissionDefault {
    /// Bukkit's own default-to-boolean resolution against op status, exactly.
    pub fn value(self, op: bool) -> bool {
        match self {
            Self::True => true,
            Self::False => false,
            Self::Op => op,
            Self::NotOp => !op,
        }
    }
}

/// Bukkit's own global default-permission fallback, which really is
/// the op-only default — the default applied to a node **no plugin
/// declared**.
///
/// Spelled out as a constant because it is the single most surprising step in
/// the resolution order (see the module doc, step 6): an undeclared node is
/// held by every operator, not denied to everyone. [`Permissions::strict`]
/// substitutes [`PermissionDefault::False`] here for callers who want
/// deny-by-default.
pub const DEFAULT_PERMISSION: PermissionDefault = PermissionDefault::Op;

/// Whether a grant allows or denies. A [`Grant::Deny`] is LuckPerms' `-node`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    Allow,
    Deny,
}

impl Grant {
    pub(crate) fn as_bool(self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Lower-case a node and trim surrounding whitespace, matching Bukkit's
/// `inName.toLowerCase()`.
///
/// Applied on **both** sides — when a grant is stored and when a query is
/// made — so that `Permissions::grant("MyPlugin.Admin")` and
/// `has(.., "myplugin.admin")` agree. Doing it on one side only is the classic
/// way this ends up half case-insensitive.
pub fn normalize_node(node: &str) -> String {
    node.trim().to_lowercase()
}

/// How specifically a stored grant key matched a queried node, and which
/// direction it points.
///
/// `specificity` is a plain score so every grant shape is compared in one
/// space (see the module doc's "how to change it"): an exact match scores
/// [`u32::MAX`], a wildcard scores its number of literal segments, and the
/// bare `*` scores 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GrantMatch {
    pub(crate) specificity: u32,
    pub(crate) grant: Grant,
    /// `true` when this grant came from the subject's own set rather than an
    /// inherited group — step 4 of the resolution order.
    pub(crate) own: bool,
}

/// Does the stored grant key `key` match the queried node `node`, and how
/// specifically?
///
/// Three shapes, and the wildcard one is the only interesting case:
/// `a.b.*` matches `a.b` itself as well as everything beneath it, which is
/// LuckPerms' behaviour and the one plugin authors rely on when they write
/// `myplugin.*` and expect it to cover the bare `myplugin` node too.
fn grant_matches(key: &str, node: &str) -> Option<u32> {
    if key == "*" {
        return Some(0);
    }
    if let Some(prefix) = key.strip_suffix(".*") {
        if node == prefix || node.starts_with(&format!("{prefix}.")) {
            // One point per literal segment, so `a.b.*` (2) outranks `a.*` (1).
            return Some(prefix.split('.').count() as u32);
        }
        return None;
    }
    if key == node {
        return Some(u32::MAX);
    }
    None
}

/// A set of node→[`Grant`] entries for one player or one group.
///
/// `BTreeMap` rather than `HashMap` so iteration order is deterministic —
/// resolution does not depend on it (the specificity score decides), but a
/// deterministic order makes a failing assertion reproducible rather than
/// flaky, which matters more than the lookup constant at these sizes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrantSet {
    entries: BTreeMap<String, Grant>,
}

impl GrantSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a grant. `node` may be exact (`a.b.c`), a suffix wildcard
    /// (`a.b.*`) or the bare `*`. A leading `-` is **not** parsed as negation
    /// here — pass [`Grant::Deny`] explicitly; see [`GrantSet::parse`] for the
    /// LuckPerms `-node` text form.
    pub fn set(&mut self, node: &str, grant: Grant) {
        self.entries.insert(normalize_node(node), grant);
    }

    pub fn allow(&mut self, node: &str) {
        self.set(node, Grant::Allow);
    }

    pub fn deny(&mut self, node: &str) {
        self.set(node, Grant::Deny);
    }

    pub fn remove(&mut self, node: &str) -> Option<Grant> {
        self.entries.remove(&normalize_node(node))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, Grant)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), *v))
    }

    /// Parse LuckPerms' text form, where a leading `-` means deny:
    /// `["myplugin.*", "-myplugin.admin"]` grants the tree and carves out the
    /// admin branch.
    pub fn parse<'a>(nodes: impl IntoIterator<Item = &'a str>) -> Self {
        let mut set = Self::new();
        for raw in nodes {
            match raw.strip_prefix('-') {
                Some(node) => set.deny(node),
                None => set.allow(raw),
            }
        }
        set
    }

    /// The best match in this set for `node`, or `None` if nothing matched.
    ///
    /// "Best" is [`better_match`]'s ordering — most specific first, then tier,
    /// then deny-over-allow. Every entry in this set shares one tier, so only
    /// specificity and negation can decide it here; the tier comparison
    /// matters when [`PermissionStore::collect_grants`] folds a player's own
    /// result together with each group's.
    pub(crate) fn best_match(&self, node: &str, own: bool) -> Option<GrantMatch> {
        let mut best: Option<GrantMatch> = None;
        for (key, grant) in &self.entries {
            let Some(specificity) = grant_matches(key, node) else {
                continue;
            };
            let candidate = GrantMatch {
                specificity,
                grant: *grant,
                own,
            };
            best = Some(match best {
                None => candidate,
                Some(current) => better_match(current, candidate),
            });
        }
        best
    }
}

/// Pick the winner between two matches, applying steps 2–4 of the resolution
/// order in that exact precedence: **specificity, then tier (own before
/// inherited), then deny-over-allow.**
///
/// Kept as one function so there is a single place the precedence lives — the
/// module doc's "how to change it" explains why a second comparison path is
/// the failure mode to avoid. The order of the last two comparisons is not
/// interchangeable: see the module doc's step 4 for why putting negation above
/// tier makes the tier rule unobservable.
pub(crate) fn better_match(a: GrantMatch, b: GrantMatch) -> GrantMatch {
    use std::cmp::Ordering;
    match a.specificity.cmp(&b.specificity) {
        Ordering::Greater => return a,
        Ordering::Less => return b,
        Ordering::Equal => {}
    }
    // Equal specificity: the subject's own grant outranks an inherited one.
    match (a.own, b.own) {
        (true, false) => return a,
        (false, true) => return b,
        _ => {}
    }
    // Same specificity and same tier: a deny wins over an allow.
    match (a.grant, b.grant) {
        (Grant::Deny, _) => a,
        (_, Grant::Deny) => b,
        _ => a,
    }
}
