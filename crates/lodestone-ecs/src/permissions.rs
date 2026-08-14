//! The permission node system: dotted nodes, wildcards,
//! per-node defaults, per-player and per-group grants with negation, vanilla's
//! five command levels, and a resolver trait a permissions *plugin* can use to
//! replace the built-in resolution entirely.
//!
//! ## What it is
//!
//! One resource, [`Permissions`], answering one question — *does this subject
//! hold this permission?* ([`Permissions::has`] for the common dotted-string
//! case, [`Permissions::check`] for the full [`Permission`] enum). Everything
//! else here exists to make that answer well-defined: [`PermissionRegistry`]
//! holds what each node defaults to when nobody granted it,
//! [`PermissionStore`] holds who was granted what, and
//! [`PermissionResolver`] is the seam that lets LuckPerms-shaped plugin take
//! the whole decision over.
//!
//! ## Two parity targets, deliberately layered
//!
//! This subsystem answers to **two** upstream models at once, and conflating
//! them is the mistake to avoid:
//!
//! - **Vanilla 26.2** has a real permission system now — it is no longer the
//!   bare numeric op level of earlier versions. Read from the
//!   26.2 jar at `.cache/mc/26.2/src/net/minecraft/server/permissions/`:
//!   `PermissionLevel` is a five-variant enum (`ALL`=0, `MODERATORS`=1,
//!   `GAMEMASTERS`=2, `ADMINS`=3, `OWNERS`=4) with `isEqualOrHigherThan`;
//!   `Permission` is a sum of `Atom(Identifier)` and
//!   `HasCommandLevel(PermissionLevel)`; `PermissionSet` is the
//!   `hasPermission(Permission) -> boolean` interface with `NO_PERMISSIONS`,
//!   `ALL_PERMISSIONS` and `union`; and `PermissionCheck` is `AlwaysPass` or
//!   `Require(Permission)`. [`PermissionLevel`] and [`Permission`] here are
//!   that model, transliterated in name and numbering so a future
//!   `ops.json`/`ClientboundCommandsPacket` consumer needs no mapping table.
//!
//! - **Bukkit/Paper** is what a *plugin author* expects, and it is a different
//!   shape: dotted string nodes, four-valued defaults, and attachments. Its
//!   resolution order is layered on top of the vanilla model rather than
//!   replacing it, exactly as the real Bukkit does (`PermissibleBase` sits on
//!   top of vanilla's op level, it does not supersede it).
//!
//! ## The resolution order, and where each step comes from
//!
//! [`Permissions::check`] resolves in this order. Steps 1 and 5–6 are Bukkit's
//! verbatim; steps 2–4 are LuckPerms', because Bukkit alone cannot answer a
//! wildcard question at all (see the next section).
//!
//! 1. **An installed [`PermissionResolver`] wins outright**, if it returns
//!    `Some`. This is the resolver trait that lets a permissions *plugin*
//!    override the built-in op-level resolver entirely, and it is checked
//!    first so a LuckPerms-equivalent really does get the whole decision. A
//!    resolver returning `None` falls through to everything below, so a plugin
//!    can also override *selectively*.
//!
//! 2. **The most specific matching grant**, across the subject's own grants
//!    and every group it inherits from. Exact beats wildcard; among wildcards,
//!    the one with more literal segments beats the one with fewer (`a.b.*`
//!    beats `a.*` beats `*`). This is LuckPerms' specificity rule.
//!
//! 3. **At equal specificity, a subject's own grant beats one inherited from
//!    a group** — LuckPerms' user-over-group weighting. So a player's own
//!    `-a.b` deny overrides their group's `a.b` allow.
//!
//! 4. **Within the same tier, a deny beats an allow.** LuckPerms documents
//!    this for the `foo.bar` / `-foo.bar` pair. It applies *after* step 3, so
//!    it is what settles two different **groups** disagreeing about the same
//!    node.
//!
//!    The ordering of steps 2–4 is load-bearing and each boundary is pinned by
//!    its own test, because getting any pair the wrong way round produces a
//!    system that looks right in the common case:
//!
//!    - specificity **before** tier: a group's exact `a.b` allow beats the
//!      player's own `a.*` deny (`group_exact_grant_beats_player_wildcard_grant`).
//!      This is the most surprising consequence of the whole order.
//!    - tier **before** negation: the player's own `-a.b` beats a group's
//!      `a.b` (`player_grant_beats_group_grant_at_equal_specificity`).
//!    - negation last: two groups, one allowing and one denying `a.b`,
//!      resolve to deny (`a_deny_beats_an_allow_at_equal_specificity`).
//!
//!    Had negation been ordered above tier, step 3 would be **unobservable**:
//!    it would only ever fire when specificity *and* direction already
//!    matched, in which case the resolved boolean is identical either way. An
//!    earlier draft of this module had exactly that bug — the step was
//!    documented, implemented, and could not change any answer.
//!
//! 5. **The node's declared default**, evaluated against op status —
//!    `PermissionDefault.getValue(boolean op)` from Bukkit, whose four values
//!    are exactly [`PermissionDefault`]'s: `TRUE`→`true`, `FALSE`→`false`,
//!    `OP`→`op`, `NOT_OP`→`!op`. A three-value description of the default —
//!    "`true`/`false`/`op`" — misses one: Bukkit has **four**, and
//!    `NOT_OP` is load-bearing for real plugins (it is how you gate a thing
//!    *away* from staff). See [`PermissionDefault`].
//!
//! 6. **An undeclared node falls back to [`DEFAULT_PERMISSION`], which is
//!    `Op`** — not `False`. This is the step most likely to surprise: Bukkit's
//!    `Permission.DEFAULT_PERMISSION` really is `PermissionDefault.OP`, so in
//!    Bukkit a node no plugin ever declared is held by every operator. We
//!    match it, because a plugin ported from Bukkit will have been written
//!    against it. [`Permissions::strict`] flips this one step to `False` for a
//!    deployment that wants deny-by-default; nothing else changes.
//!
//! Source for steps 1 and 5–6, quoted from `PermissibleBase.hasPermission`
//! (Bukkit master, `org.bukkit.permissions.PermissibleBase`):
//!
//! ```text
//! String name = inName.toLowerCase();
//! if (isPermissionSet(name)) {
//!     return permissions.get(name).getValue();
//! } else {
//!     Permission perm = Bukkit.getServer().getPluginManager().getPermission(name);
//!     if (perm != null) {
//!         return perm.getDefault().getValue(isOp());
//!     } else {
//!         return Permission.DEFAULT_PERMISSION.getValue(isOp());
//!     }
//! }
//! ```
//!
//! Note the `toLowerCase()`: Bukkit permission nodes are **case-insensitive**,
//! and so are these — every node is normalised through
//! [`normalize_node`] on both the grant and the query side.
//!
//! ## Where we knowingly diverge from Bukkit, and why
//!
//! **Bukkit does not match wildcards at check time at all.** Its
//! `PermissibleBase.hasPermission` is an exact `HashMap` lookup;
//! `myplugin.*` only works in Bukkit because a plugin *declares* that
//! permission with `getChildren()`, and `calculateChildPermissions` flattens
//! those children into the attachment map when the permission is **set**. That
//! design cannot answer "does `myplugin.admin.reload` match the `myplugin.*`
//! this player holds?" for a node nobody declared in advance — which is
//! precisely the "wildcard suffix matching" this module exists to answer.
//!
//! So wildcards here are resolved at *check* time, LuckPerms-style, which is
//! also what almost every real server's authors are actually used to: in
//! practice, on almost every real server, a permissions
//! *plugin* like LuckPerms is layered on top. The consequence to know: a
//! wildcard grant here matches nodes that no plugin declared, where in bare
//! Bukkit it would not.
//!
//! **Vanilla's built-in resolver denies atoms, and is stricter than "a
//! minimal built-in resolver (op = everything, non-op = only nodes explicitly
//! defaulted true)" would be.** Vanilla 26.2's
//! `LevelBasedPermissionSet.hasPermission` does *not* do that — an
//! `Atom` permission returns `false` for **every** level except the one
//! hardcoded case `COMMANDS_ENTITY_SELECTORS` (which requires `GAMEMASTERS`):
//!
//! ```text
//! if (permission instanceof Permission.HasCommandLevel levelCheck) {
//!    return this.level().isEqualOrHigherThan(levelCheck.level());
//! } else {
//!    return permission.equals(Permissions.COMMANDS_ENTITY_SELECTORS)
//!       ? this.level().isEqualOrHigherThan(PermissionLevel.GAMEMASTERS)
//!       : false;
//! }
//! ```
//!
//! We follow **Bukkit** rather than vanilla for atoms, because the consumer is
//! a plugin API: an op does hold an undeclared atom here (step 6), where
//! vanilla's level-based set would deny it. [`LevelBasedPermissionSet`] is
//! vanilla's behaviour, available exactly, for a caller that wants host parity
//! instead — and `vanilla_level_set_denies_an_undeclared_atom_where_bukkit_grants_it`
//! is the test that pins the two apart so nobody "fixes" one into the other.
//!
//! ## How to change it
//!
//! - **Adding a resolution step** means editing [`Permissions::check`] and the
//!   order list above together. The order is the specification; a step added
//!   in code and not in the doc is the staleness this repo's `CLAUDE.md`
//!   names as its most common defect.
//! - **Group inheritance is depth-first with a visited set**
//!   ([`PermissionStore::collect_grants`]), so a cyclic group graph
//!   terminates rather than hanging. `cyclic_group_inheritance_terminates`
//!   is the guard; do not "simplify" the visited set away.
//! - **Specificity is a `u32` score, not a comparison function**
//!   ([`GrantMatch::specificity`]). If you add a new grant shape (a regex, a
//!   negated wildcard), give it a score in the same space and extend
//!   [`grant_matches`]; a second comparison path is how two callers start
//!   disagreeing about which grant wins.
//! - **Nothing here touches the network.** Op level is not on the wire in any
//!   protocol family in this workspace (verified: `AbilitiesChanged` carries
//!   six fields, none of them a level), so [`PermissionStore::set_level`] is
//!   the only way a level is ever set today. A future `ops.json` loader or
//!   `ClientboundCommandsPacket` consumer is its caller.
//!
//! ## Configuration
//!
//! None — no env vars, no files. [`Permissions::default`] is an empty registry
//! with no grants and no resolver, which by step 6 means *ops hold every node
//! and nobody else holds any*: vanilla's op/non-op split, which keeps the
//! system usable with zero permission plugins installed.
//!
//! ## Dependencies
//!
//! `bevy_ecs` for `Resource`, `uuid` for the subject id. Deliberately not
//! `lodestone-command` — a permission is a string to this module, and the
//! command tree's per-node gating is the *caller's* join of the
//! two, in [`crate::commands`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use bevy_ecs::resource::Resource;
use uuid::Uuid;

/// Vanilla's five command levels, transliterated from 26.2's
/// `net.minecraft.server.permissions.PermissionLevel` — same variants, same
/// ids, same `isEqualOrHigherThan`.
///
/// A description of "four op levels (2-4 for the built-in
/// commands, plus the `op`/non-op boolean)" misses one. There are **five** (0 through 4),
/// and `ALL`=0 is the level a non-op holds rather than the absence of a level;
/// `ops.json`'s `level` field is exactly this id
/// (`ServerOpListEntry`: `PermissionLevel.byId(object.get("level").getAsInt())`).
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
    /// `PermissionLevel.id()`.
    pub fn id(self) -> u8 {
        match self {
            Self::All => 0,
            Self::Moderators => 1,
            Self::Gamemasters => 2,
            Self::Admins => 3,
            Self::Owners => 4,
        }
    }

    /// Vanilla's `PermissionLevel.getSerializedName()`.
    pub fn serialized_name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Moderators => "moderators",
            Self::Gamemasters => "gamemasters",
            Self::Admins => "admins",
            Self::Owners => "owners",
        }
    }

    /// Vanilla's `PermissionLevel.byId`, **including its clamping**: the
    /// upstream is `ByIdMap.continuous(..., OutOfBoundsStrategy.CLAMP)`, so an
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

    /// Vanilla's `PermissionLevel.isEqualOrHigherThan`.
    pub fn is_equal_or_higher_than(self, other: Self) -> bool {
        self.id() >= other.id()
    }

    /// Whether this level counts as "op" for Bukkit's `isOp()`, which is what
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

/// Vanilla's `net.minecraft.server.permissions.Permission` — a sum of a
/// namespaced atom and a command-level requirement.
///
/// Both variants exist because vanilla really does mix them in one interface:
/// `Permissions.COMMANDS_GAMEMASTER` is a `HasCommandLevel`, while
/// `Permissions.CHAT_SEND_COMMANDS` is an `Atom("chat/send_commands")`. A
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

/// Bukkit's `org.bukkit.permissions.PermissionDefault` — what a *declared*
/// node resolves to when no grant matched.
///
/// All four values, with Bukkit's exact `getValue(boolean op)` table. A
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
    /// Bukkit's `PermissionDefault.getValue(boolean op)`, exactly.
    pub fn value(self, op: bool) -> bool {
        match self {
            Self::True => true,
            Self::False => false,
            Self::Op => op,
            Self::NotOp => !op,
        }
    }
}

/// Bukkit's `Permission.DEFAULT_PERMISSION`, which really is
/// `PermissionDefault.OP` — the default applied to a node **no plugin
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
    fn as_bool(self) -> bool {
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
struct GrantMatch {
    specificity: u32,
    grant: Grant,
    /// `true` when this grant came from the subject's own set rather than an
    /// inherited group — step 4 of the resolution order.
    own: bool,
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
    fn best_match(&self, node: &str, own: bool) -> Option<GrantMatch> {
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
fn better_match(a: GrantMatch, b: GrantMatch) -> GrantMatch {
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
/// [`PermissionSubject::Console`] is vanilla's `PermissionSet.ALL_PERMISSIONS`
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
    fn collect_grants(&self, player: Uuid, node: &str) -> Option<GrantMatch> {
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

/// What each declared node defaults to — a per-node default.
#[derive(Debug, Default)]
pub struct PermissionRegistry {
    declared: HashMap<String, PermissionDefault>,
    descriptions: HashMap<String, String>,
}

impl PermissionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a node and its default, as a Bukkit plugin's `plugin.yml`
    /// `permissions:` block does.
    pub fn declare(&mut self, node: &str, default: PermissionDefault) {
        self.declared.insert(normalize_node(node), default);
    }

    /// Declare a node with a human description, for a future `/permissions`
    /// listing. The description is stored and never consulted by resolution.
    pub fn declare_described(
        &mut self,
        node: &str,
        default: PermissionDefault,
        description: impl Into<String>,
    ) {
        let node = normalize_node(node);
        self.declared.insert(node.clone(), default);
        self.descriptions.insert(node, description.into());
    }

    /// The declared default, or `None` if this node was never declared — which
    /// step 6 turns into [`DEFAULT_PERMISSION`].
    pub fn default_for(&self, node: &str) -> Option<PermissionDefault> {
        self.declared.get(&normalize_node(node)).copied()
    }

    pub fn description(&self, node: &str) -> Option<&str> {
        self.descriptions.get(&normalize_node(node)).map(|s| s.as_str())
    }

    pub fn declared_nodes(&self) -> impl Iterator<Item = (&str, PermissionDefault)> {
        self.declared.iter().map(|(k, v)| (k.as_str(), *v))
    }
}

/// Everything the built-in resolution needs to answer one query, handed to an
/// installed [`PermissionResolver`] so a plugin can make its own decision from
/// the same inputs.
pub struct PermissionQuery<'a> {
    pub subject: PermissionSubject,
    pub permission: &'a Permission,
    /// The subject's command level. [`PermissionLevel::Owners`] for
    /// [`PermissionSubject::Console`].
    pub level: PermissionLevel,
    pub registry: &'a PermissionRegistry,
    pub store: &'a PermissionStore,
}

impl std::fmt::Debug for PermissionQuery<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionQuery")
            .field("subject", &self.subject)
            .field("permission", &self.permission)
            .field("level", &self.level)
            .finish_non_exhaustive()
    }
}

impl PermissionQuery<'_> {
    /// The node string, for the [`Permission::Atom`] case. `None` for a
    /// [`Permission::HasCommandLevel`] query, which has no node.
    pub fn node(&self) -> Option<&str> {
        match self.permission {
            Permission::Atom(node) => Some(node.as_str()),
            Permission::HasCommandLevel(_) => None,
        }
    }
}

/// The seam this module provides: a resolver trait so a permissions *plugin*
/// (matching the real-world pattern of delegating to LuckPerms) can override
/// the built-in op-level resolver entirely.
///
/// Returning `Some(bool)` decides the query outright. Returning `None` falls
/// through to the built-in order, so a plugin can own only the nodes it cares
/// about — a full takeover returns `Some` unconditionally.
pub trait PermissionResolver: Send + Sync {
    fn resolve(&self, query: &PermissionQuery<'_>) -> Option<bool>;
}

impl<F> PermissionResolver for F
where
    F: Fn(&PermissionQuery<'_>) -> Option<bool> + Send + Sync,
{
    fn resolve(&self, query: &PermissionQuery<'_>) -> Option<bool> {
        self(query)
    }
}

/// Vanilla's `LevelBasedPermissionSet`, exactly — provided so a caller that
/// wants *host* parity rather than *plugin* parity can have it.
///
/// This is **not** what [`Permissions`] does for atoms, and the difference is
/// deliberate: vanilla denies every atom except
/// `minecraft:commands/entity_selectors`, where Bukkit grants an undeclared
/// atom to any op. See the module doc's divergence section.
#[derive(Debug, Clone, Copy)]
pub struct LevelBasedPermissionSet {
    pub level: PermissionLevel,
}

impl LevelBasedPermissionSet {
    /// Vanilla's `LevelBasedPermissionSet.forLevel`.
    pub fn for_level(level: PermissionLevel) -> Self {
        Self { level }
    }

    /// Vanilla's node id for the one atom its level-based set special-cases:
    /// `Permissions.COMMANDS_ENTITY_SELECTORS = Permission.Atom.create("commands/entity_selectors")`,
    /// which an `Identifier` with the default namespace renders as
    /// `minecraft:commands/entity_selectors`. Both spellings are accepted
    /// because vanilla's own constant omits the namespace at the call site.
    pub const COMMANDS_ENTITY_SELECTORS: &'static str = "commands/entity_selectors";

    /// Vanilla's `LevelBasedPermissionSet.hasPermission`, transliterated.
    pub fn has_permission(&self, permission: &Permission) -> bool {
        match permission {
            Permission::HasCommandLevel(required) => self.level.is_equal_or_higher_than(*required),
            Permission::Atom(node) => {
                let node = normalize_node(node);
                let is_selectors = node == Self::COMMANDS_ENTITY_SELECTORS
                    || node == format!("minecraft:{}", Self::COMMANDS_ENTITY_SELECTORS);
                is_selectors && self.level.is_equal_or_higher_than(PermissionLevel::Gamemasters)
            }
        }
    }

    /// Two level-based sets collapse to the **higher** level rather than
    /// forming a union object.
    ///
    /// # A deliberate deviation from vanilla's literal code
    ///
    /// 26.2's `LevelBasedPermissionSet.union` reads:
    ///
    /// ```text
    /// return this.level().isEqualOrHigherThan(otherSet.level()) ? otherSet : this;
    /// ```
    ///
    /// which returns the **lower**-level set when `this` is the higher one.
    /// That contradicts what `union` means everywhere else in the same file —
    /// `PermissionSetUnion.hasPermission` returns `true` if **any** member set
    /// holds the permission, i.e. a logical OR — so a union that *narrows* is
    /// inconsistent with its own interface. We implement the OR-consistent
    /// behaviour (keep the higher level) rather than transliterate what looks
    /// like an upstream bug, because a caller composing two sets and getting
    /// fewer permissions than either one had would be indefensible here even
    /// if it is what the jar does.
    ///
    /// This is the one place in this module that knowingly does not match the
    /// jar. If a live-oracle measurement ever shows vanilla's behaviour is
    /// load-bearing, flip it here and update this doc together —
    /// `vanilla_level_set_union_keeps_the_higher_level` is the test to change.
    pub fn union(self, other: Self) -> Self {
        if self.level.is_equal_or_higher_than(other.level) {
            self
        } else {
            other
        }
    }
}

/// The one resource a plugin asks. See the module doc for the resolution
/// order — that list is this type's specification.
#[derive(Resource, Default)]
pub struct Permissions {
    pub registry: PermissionRegistry,
    pub store: PermissionStore,
    resolver: Option<Arc<dyn PermissionResolver>>,
    /// Substituted for [`DEFAULT_PERMISSION`] at step 6. `None` means Bukkit's
    /// `Op`; [`Permissions::strict`] sets it to `False`.
    undeclared_default: Option<PermissionDefault>,
}

impl std::fmt::Debug for Permissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Permissions")
            .field("registry", &self.registry)
            .field("store", &self.store)
            .field("resolver", &self.resolver.as_ref().map(|_| "<dyn PermissionResolver>"))
            .field("undeclared_default", &self.undeclared_default)
            .finish()
    }
}

impl Permissions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Deny-by-default: an **undeclared** node resolves to `false` for
    /// everyone, including ops, instead of Bukkit's `Op`.
    ///
    /// Only step 6 changes. A declared node still uses its own default, and
    /// grants still win over both.
    pub fn strict() -> Self {
        Self {
            undeclared_default: Some(PermissionDefault::False),
            ..Self::default()
        }
    }

    /// Install a [`PermissionResolver`] that gets first refusal on every
    /// query. Replaces any previously installed one — there is deliberately no
    /// resolver *chain*, because two plugins silently disagreeing about a node
    /// is worse than one plugin obviously winning.
    pub fn set_resolver(&mut self, resolver: Arc<dyn PermissionResolver>) {
        self.resolver = Some(resolver);
    }

    pub fn clear_resolver(&mut self) {
        self.resolver = None;
    }

    pub fn has_resolver(&self) -> bool {
        self.resolver.is_some()
    }

    /// Declare a node's default. Convenience for `self.registry.declare`.
    pub fn declare(&mut self, node: &str, default: PermissionDefault) {
        self.registry.declare(node, default);
    }

    /// Grant a node to a player. Convenience for the store.
    pub fn grant(&mut self, player: Uuid, node: &str) {
        self.store.subject_mut(player).grants.allow(node);
    }

    /// Deny a node to a player (LuckPerms' `-node`).
    pub fn deny(&mut self, player: Uuid, node: &str) {
        self.store.subject_mut(player).grants.deny(node);
    }

    /// The common case: does this subject hold this dotted node?
    pub fn has(&self, subject: PermissionSubject, node: &str) -> bool {
        self.check(subject, &Permission::Atom(node.to_string()))
    }

    /// Is this subject at `level` or higher? The op-level accessor,
    /// expressed through the same resolution order so an installed resolver
    /// can override *this* too — a LuckPerms-equivalent that wants to grant
    /// gamemaster powers by node rather than by op list can.
    pub fn has_level(&self, subject: PermissionSubject, level: PermissionLevel) -> bool {
        self.check(subject, &Permission::HasCommandLevel(level))
    }

    /// The subject's command level. `Owners` for the console.
    pub fn level(&self, subject: PermissionSubject) -> PermissionLevel {
        match subject {
            PermissionSubject::Console => PermissionLevel::Owners,
            PermissionSubject::Player(id) => self.store.level(id),
        }
    }

    /// The full resolution, in the order the module doc specifies.
    pub fn check(&self, subject: PermissionSubject, permission: &Permission) -> bool {
        let level = self.level(subject);

        // Step 1 — an installed resolver gets first refusal, for every
        // subject including the console. A permissions plugin that wants to
        // deny the console something must be able to.
        if let Some(resolver) = &self.resolver {
            let query = PermissionQuery {
                subject,
                permission,
                level,
                registry: &self.registry,
                store: &self.store,
            };
            if let Some(decided) = resolver.resolve(&query) {
                return decided;
            }
        }

        // The console holds everything — vanilla's `ALL_PERMISSIONS`.
        if matches!(subject, PermissionSubject::Console) {
            return true;
        }

        // A level query is answered by the level alone. There is no node to
        // match a grant against, and no default to consult.
        let node = match permission {
            Permission::HasCommandLevel(required) => {
                return level.is_equal_or_higher_than(*required);
            }
            Permission::Atom(node) => normalize_node(node),
        };

        // Steps 2–4 — the most specific matching grant, deny-over-allow at
        // equal specificity, own-over-inherited after that.
        if let PermissionSubject::Player(id) = subject {
            if let Some(matched) = self.store.collect_grants(id, &node) {
                return matched.grant.as_bool();
            }
        }

        // Steps 5–6 — the declared default, else the undeclared fallback.
        let default = self
            .registry
            .default_for(&node)
            .or(self.undeclared_default)
            .unwrap_or(DEFAULT_PERMISSION);
        default.value(level.is_op())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player() -> PermissionSubject {
        PermissionSubject::Player(Uuid::from_u128(1))
    }

    fn player_id() -> Uuid {
        Uuid::from_u128(1)
    }

    // ---------------------------------------------------------------------
    // Vanilla parity: PermissionLevel
    // ---------------------------------------------------------------------

    /// The five ids match vanilla's `PermissionLevel` enum exactly. Written as
    /// a table rather than five asserts so a reordering of the enum shows up
    /// as one failure naming the wrong pair.
    #[test]
    fn permission_level_ids_match_vanilla() {
        let table = [
            (PermissionLevel::All, 0, "all"),
            (PermissionLevel::Moderators, 1, "moderators"),
            (PermissionLevel::Gamemasters, 2, "gamemasters"),
            (PermissionLevel::Admins, 3, "admins"),
            (PermissionLevel::Owners, 4, "owners"),
        ];
        for (level, id, name) in table {
            assert_eq!(level.id(), id, "id for {level:?}");
            assert_eq!(level.serialized_name(), name, "name for {level:?}");
            assert_eq!(PermissionLevel::by_id(i32::from(id)), level, "by_id({id})");
        }
    }

    /// `by_id` clamps, because vanilla's is
    /// `ByIdMap.continuous(..., OutOfBoundsStrategy.CLAMP)`. A hand-edited
    /// `ops.json` with `"level": 99` means OWNERS upstream, and must here.
    #[test]
    fn permission_level_by_id_clamps_out_of_range() {
        assert_eq!(PermissionLevel::by_id(99), PermissionLevel::Owners);
        assert_eq!(PermissionLevel::by_id(5), PermissionLevel::Owners);
        assert_eq!(PermissionLevel::by_id(-7), PermissionLevel::All);
    }

    /// The op boundary is level 1, not level 0 — `ops.json` cannot record a
    /// non-op, so "in the file" and ">= MODERATORS" are the same condition.
    #[test]
    fn only_level_one_and_up_counts_as_op() {
        assert!(!PermissionLevel::All.is_op());
        assert!(PermissionLevel::Moderators.is_op());
        assert!(PermissionLevel::Owners.is_op());
    }

    // ---------------------------------------------------------------------
    // Bukkit parity: PermissionDefault
    // ---------------------------------------------------------------------

    /// Bukkit's `PermissionDefault.getValue(boolean op)` table, all four
    /// values against both op states. Eight cells, because the interesting
    /// value (`NotOp`) is the one a three-value description of the default
    /// omits.
    #[test]
    fn permission_default_matches_bukkit_get_value_table() {
        let table = [
            (PermissionDefault::True, true, true),
            (PermissionDefault::True, false, true),
            (PermissionDefault::False, true, false),
            (PermissionDefault::False, false, false),
            (PermissionDefault::Op, true, true),
            (PermissionDefault::Op, false, false),
            (PermissionDefault::NotOp, true, false),
            (PermissionDefault::NotOp, false, true),
        ];
        for (default, op, expected) in table {
            assert_eq!(default.value(op), expected, "{default:?}.value({op})");
        }
    }

    /// Bukkit's `Permission.DEFAULT_PERMISSION` is `OP`, so an undeclared node
    /// is held by an op and not by anyone else. This is the step most likely
    /// to be "corrected" to `False` by someone who has not read
    /// `PermissibleBase`, so it is pinned directly.
    #[test]
    fn an_undeclared_node_is_held_by_ops_and_nobody_else() {
        let mut permissions = Permissions::new();
        assert!(
            !permissions.has(player(), "nobody.declared.this"),
            "a non-op must not hold an undeclared node"
        );

        permissions
            .store
            .set_level(player_id(), PermissionLevel::Gamemasters);
        assert!(
            permissions.has(player(), "nobody.declared.this"),
            "an op must hold an undeclared node — Bukkit's DEFAULT_PERMISSION is OP"
        );
    }

    /// The negative control for the test above, and the reason
    /// [`Permissions::strict`] exists: with strict mode on, the *same* op no
    /// longer holds the *same* undeclared node. Without this pair, a bug that
    /// made `has` always return `true` for ops would look correct above.
    #[test]
    fn strict_mode_denies_an_undeclared_node_even_to_an_owner() {
        let mut permissions = Permissions::strict();
        permissions
            .store
            .set_level(player_id(), PermissionLevel::Owners);
        assert!(!permissions.has(player(), "nobody.declared.this"));

        // ... and strict mode changes *only* step 6: a declared `Op` node is
        // still held by the same owner, so the flag is narrow rather than a
        // blanket deny.
        permissions.declare("declared.node", PermissionDefault::Op);
        assert!(permissions.has(player(), "declared.node"));
    }

    /// A declared default is consulted before the undeclared fallback, and
    /// `True` really does reach a non-op.
    #[test]
    fn a_declared_true_default_reaches_a_non_op() {
        let mut permissions = Permissions::new();
        permissions.declare("myplugin.help", PermissionDefault::True);
        assert!(permissions.has(player(), "myplugin.help"));
        assert_eq!(permissions.level(player()), PermissionLevel::All);
    }

    /// `NotOp` inverts, which is the whole reason the fourth value exists.
    #[test]
    fn a_not_op_default_is_held_by_a_non_op_and_lost_when_opped() {
        let mut permissions = Permissions::new();
        permissions.declare("myplugin.hints", PermissionDefault::NotOp);
        assert!(permissions.has(player(), "myplugin.hints"));

        permissions
            .store
            .set_level(player_id(), PermissionLevel::Admins);
        assert!(!permissions.has(player(), "myplugin.hints"));
    }

    // ---------------------------------------------------------------------
    // Wildcards and specificity
    // ---------------------------------------------------------------------

    /// A wildcard grant covers the subtree *and* the bare prefix node, which
    /// is LuckPerms' behaviour and what `myplugin.*` is expected to mean.
    #[test]
    fn a_wildcard_grant_covers_the_subtree_and_the_bare_prefix() {
        let mut permissions = Permissions::new();
        permissions.declare("myplugin.admin.reload", PermissionDefault::Op);
        permissions.grant(player_id(), "myplugin.*");

        assert!(permissions.has(player(), "myplugin.admin.reload"));
        assert!(permissions.has(player(), "myplugin.anything.deeper.still"));
        assert!(permissions.has(player(), "myplugin"), "the bare prefix too");
    }

    /// The control for the wildcard test: a *neighbouring* tree is not
    /// covered. Without this, a `grant_matches` that returned `Some` for
    /// everything would pass the test above.
    #[test]
    fn a_wildcard_grant_does_not_leak_into_a_neighbouring_tree() {
        let mut permissions = Permissions::new();
        permissions.grant(player_id(), "myplugin.*");
        assert!(!permissions.has(player(), "otherplugin.admin"));
        // Nor a node that merely shares a textual prefix without a segment
        // boundary — `myplugin` must not match `myplugintwo`.
        assert!(!permissions.has(player(), "myplugintwo.admin"));
    }

    /// A more specific deny carves a hole in a broader allow — the
    /// `["myplugin.*", "-myplugin.admin"]` shape every real permissions
    /// config uses.
    #[test]
    fn a_specific_deny_carves_a_hole_in_a_wildcard_allow() {
        let mut permissions = Permissions::new();
        permissions.grant(player_id(), "myplugin.*");
        permissions.deny(player_id(), "myplugin.admin");

        assert!(permissions.has(player(), "myplugin.help"));
        assert!(!permissions.has(player(), "myplugin.admin"));

        // **The gotcha.** An *exact* deny does not cover its children: the only
        // key matching `myplugin.admin.reload` is the `myplugin.*` allow, so
        // the child is still permitted. To carve out a whole branch you must
        // deny the wildcard (`-myplugin.admin.*`), which is exactly LuckPerms'
        // behaviour and the mistake most permissions configs make once.
        assert!(
            permissions.has(player(), "myplugin.admin.reload"),
            "an exact deny must NOT cover children — only the wildcard form does"
        );

        permissions.deny(player_id(), "myplugin.admin.*");
        assert!(
            !permissions.has(player(), "myplugin.admin.reload"),
            "denying the wildcard form does cover the branch"
        );
    }

    /// Longer wildcards outrank shorter ones, and the bare `*` is the weakest
    /// grant there is.
    #[test]
    fn wildcard_specificity_is_ordered_by_literal_segment_count() {
        let mut permissions = Permissions::new();
        permissions.store.subject_mut(player_id()).grants =
            GrantSet::parse(["*", "-a.*", "a.b.*"]);

        assert!(permissions.has(player(), "z.anything"), "bare * allows");
        assert!(!permissions.has(player(), "a.other"), "-a.* is more specific");
        assert!(permissions.has(player(), "a.b.c"), "a.b.* is more specific still");
    }

    /// Step 4: within the same tier, a deny wins. Two **groups** are used
    /// deliberately — same tier, same specificity, opposite directions — since
    /// that is the only situation step 4 decides once step 3 has had its turn.
    /// Built by hand rather than through `grant`/`deny`, which would overwrite
    /// the same key in one set.
    #[test]
    fn a_deny_beats_an_allow_at_equal_specificity() {
        let mut permissions = Permissions::new();
        permissions.store.group_mut("staff").grants.deny("myplugin.admin");
        permissions.store.add_to_group(player_id(), "staff");
        permissions.store.group_mut("mods").grants.allow("myplugin.admin");
        permissions.store.add_to_group(player_id(), "mods");

        assert!(!permissions.has(player(), "myplugin.admin"));
    }

    // ---------------------------------------------------------------------
    // Groups
    // ---------------------------------------------------------------------

    /// A group grant reaches its members, and group inheritance is
    /// transitive.
    #[test]
    fn group_grants_are_inherited_transitively() {
        let mut permissions = Permissions::new();
        permissions.store.group_mut("owner").grants.allow("myplugin.reload");
        permissions.store.group_mut("admin").parents.push("owner".into());
        permissions.store.group_mut("staff").parents.push("admin".into());
        permissions.store.add_to_group(player_id(), "staff");

        assert!(
            permissions.has(player(), "myplugin.reload"),
            "staff -> admin -> owner must reach owner's grant"
        );
    }

    /// The control for inheritance: a player in no group does not get the
    /// grant. Without this, a `collect_grants` that scanned every group in the
    /// store regardless of membership would pass the test above.
    #[test]
    fn a_group_grant_does_not_reach_a_non_member() {
        let mut permissions = Permissions::new();
        permissions.store.group_mut("staff").grants.allow("myplugin.reload");
        permissions.declare("myplugin.reload", PermissionDefault::False);

        assert!(!permissions.has(player(), "myplugin.reload"));
    }

    /// A cyclic group graph terminates. The assertion is that the call
    /// *returns* — a missing visited set hangs or overflows the stack rather
    /// than returning a wrong answer, so this is a termination test, and the
    /// resolved value is checked too so it is not merely "it did not hang".
    #[test]
    fn cyclic_group_inheritance_terminates() {
        let mut permissions = Permissions::new();
        permissions.store.group_mut("a").parents.push("b".into());
        permissions.store.group_mut("b").parents.push("c".into());
        permissions.store.group_mut("c").parents.push("a".into());
        permissions.store.group_mut("c").grants.allow("deep.node");
        permissions.store.add_to_group(player_id(), "a");

        assert!(permissions.has(player(), "deep.node"));
    }

    /// A default group reaches a player nobody explicitly added — LuckPerms'
    /// `default` group.
    #[test]
    fn a_default_group_reaches_every_player() {
        let mut permissions = Permissions::new();
        permissions.declare("myplugin.basic", PermissionDefault::False);
        permissions.store.group_mut("default").grants.allow("myplugin.basic");
        permissions.store.add_default_group("default");

        assert!(permissions.has(player(), "myplugin.basic"));
    }

    /// The surprising consequence of comparing specificity *before*
    /// own-over-inherited, called out in the module doc's step 4 so nobody
    /// discovers it by accident: a group's **exact** allow beats the player's
    /// own **wildcard** deny.
    #[test]
    fn group_exact_grant_beats_player_wildcard_grant() {
        let mut permissions = Permissions::new();
        permissions.deny(player_id(), "myplugin.*");
        permissions.store.group_mut("staff").grants.allow("myplugin.admin");
        permissions.store.add_to_group(player_id(), "staff");

        assert!(
            permissions.has(player(), "myplugin.admin"),
            "exact beats wildcard regardless of which subject it came from"
        );
        assert!(
            !permissions.has(player(), "myplugin.other"),
            "and the player's own wildcard deny still covers everything else"
        );
    }

    /// Step 3: at equal specificity the subject's own grant outranks an
    /// inherited one, **including when the two disagree in direction**. This is
    /// the assertion that makes step 3 observable at all — see the module doc's
    /// step 4 for why an earlier ordering left it dead.
    #[test]
    fn player_grant_beats_group_grant_at_equal_specificity() {
        // Own deny vs inherited allow: the deny wins because it is the
        // player's own, not because it is a deny.
        let mut permissions = Permissions::new();
        permissions.deny(player_id(), "myplugin.admin");
        permissions.store.group_mut("staff").grants.allow("myplugin.admin");
        permissions.store.add_to_group(player_id(), "staff");
        assert!(!permissions.has(player(), "myplugin.admin"));

        // The mirror image, which is the half that distinguishes step 3 from
        // step 4: own **allow** vs inherited **deny** resolves to allow. If
        // negation were compared before tier, this would be `false`.
        let mut mirrored = Permissions::new();
        mirrored.grant(player_id(), "myplugin.admin");
        mirrored.store.group_mut("staff").grants.deny("myplugin.admin");
        mirrored.store.add_to_group(player_id(), "staff");
        assert!(
            mirrored.has(player(), "myplugin.admin"),
            "an own allow must beat an inherited deny — tier is compared before negation"
        );
    }

    // ---------------------------------------------------------------------
    // Case insensitivity
    // ---------------------------------------------------------------------

    /// Bukkit lowercases the node on both the set and the check side. Doing it
    /// on one side only is the classic half-case-insensitive bug, so both
    /// directions are exercised.
    #[test]
    fn nodes_are_case_insensitive_on_both_sides() {
        let mut permissions = Permissions::new();
        permissions.grant(player_id(), "MyPlugin.Admin");
        assert!(permissions.has(player(), "myplugin.admin"));

        let mut other = Permissions::new();
        other.grant(player_id(), "myplugin.admin");
        assert!(other.has(player(), "MYPLUGIN.ADMIN"));

        let mut declared = Permissions::new();
        declared.declare("MyPlugin.Help", PermissionDefault::True);
        assert!(declared.has(player(), "myplugin.help"));
    }

    // ---------------------------------------------------------------------
    // Levels through the resolution order
    // ---------------------------------------------------------------------

    /// A `HasCommandLevel` query is answered by the level, and is not affected
    /// by node grants — there is no node to match.
    #[test]
    fn a_level_query_is_answered_by_the_level_alone() {
        let mut permissions = Permissions::new();
        permissions
            .store
            .set_level(player_id(), PermissionLevel::Gamemasters);

        assert!(permissions.has_level(player(), PermissionLevel::Moderators));
        assert!(permissions.has_level(player(), PermissionLevel::Gamemasters));
        assert!(!permissions.has_level(player(), PermissionLevel::Admins));
    }

    /// The console holds everything, at every level — vanilla's
    /// `ALL_PERMISSIONS`.
    #[test]
    fn the_console_holds_everything() {
        let permissions = Permissions::new();
        assert!(permissions.has(PermissionSubject::Console, "anything.at.all"));
        assert!(permissions.has_level(PermissionSubject::Console, PermissionLevel::Owners));
        assert_eq!(
            permissions.level(PermissionSubject::Console),
            PermissionLevel::Owners
        );
    }

    /// Even an explicit deny does not stop the console, because step 6's
    /// short-circuit precedes grant matching. Pinned so the ordering is a
    /// decision rather than an accident.
    #[test]
    fn an_explicit_deny_does_not_stop_the_console() {
        let mut permissions = Permissions::new();
        // Deny it to *everyone* the only way the store can express.
        permissions.store.add_default_group("default");
        permissions.store.group_mut("default").grants.deny("*");
        assert!(permissions.has(PermissionSubject::Console, "anything"));
        assert!(!permissions.has(player(), "anything"));
    }

    // ---------------------------------------------------------------------
    // The resolver seam
    // ---------------------------------------------------------------------

    /// An installed resolver wins over everything, including an explicit deny
    /// and including the console short-circuit.
    #[test]
    fn an_installed_resolver_overrides_the_built_in_order() {
        let mut permissions = Permissions::new();
        permissions.deny(player_id(), "myplugin.admin");
        assert!(!permissions.has(player(), "myplugin.admin"));

        permissions.set_resolver(Arc::new(|_q: &PermissionQuery<'_>| Some(true)));
        assert!(
            permissions.has(player(), "myplugin.admin"),
            "a total-takeover resolver must beat an explicit deny"
        );
        assert!(permissions.has_resolver());

        // And it beats the console short-circuit in the other direction.
        permissions.set_resolver(Arc::new(|_q: &PermissionQuery<'_>| Some(false)));
        assert!(!permissions.has(PermissionSubject::Console, "anything"));
    }

    /// A resolver returning `None` falls through to the built-in order — the
    /// property that makes selective override possible. The control that the
    /// resolver was actually *consulted* is the counter: without it, a
    /// resolver that was never called would produce the same booleans.
    #[test]
    fn a_resolver_returning_none_falls_through_and_was_still_consulted() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);

        let mut permissions = Permissions::new();
        permissions.declare("myplugin.help", PermissionDefault::True);
        permissions.set_resolver(Arc::new(move |_q: &PermissionQuery<'_>| {
            seen.fetch_add(1, Ordering::SeqCst);
            None
        }));

        assert!(
            permissions.has(player(), "myplugin.help"),
            "fall-through must reach the declared True default"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the resolver must actually have been consulted"
        );
    }

    /// A resolver can decide *selectively*, using the query's own node and the
    /// registry/store it is handed.
    #[test]
    fn a_resolver_can_decide_only_its_own_nodes() {
        let mut permissions = Permissions::new();
        permissions.declare("other.node", PermissionDefault::True);
        permissions.set_resolver(Arc::new(|q: &PermissionQuery<'_>| {
            match q.node() {
                Some(node) if node.starts_with("luckperms.") => Some(true),
                _ => None,
            }
        }));

        assert!(permissions.has(player(), "luckperms.anything"));
        assert!(permissions.has(player(), "other.node"), "fell through");
        assert!(!permissions.has(player(), "undeclared.node"), "fell through to Op");
    }

    /// The query really carries the level and the subject, so a resolver can
    /// implement its own op logic rather than only pattern-matching nodes.
    #[test]
    fn the_query_carries_the_subject_and_its_level() {
        let mut permissions = Permissions::new();
        permissions
            .store
            .set_level(player_id(), PermissionLevel::Admins);
        permissions.set_resolver(Arc::new(|q: &PermissionQuery<'_>| {
            Some(q.level == PermissionLevel::Admins && matches!(q.subject, PermissionSubject::Player(_)))
        }));
        assert!(permissions.has(player(), "whatever"));
        assert!(!permissions.has(PermissionSubject::Console, "whatever"));
    }

    // ---------------------------------------------------------------------
    // Vanilla's level-based set, kept distinct on purpose
    // ---------------------------------------------------------------------

    /// Vanilla's `LevelBasedPermissionSet` answers a level check by level.
    #[test]
    fn vanilla_level_set_answers_a_level_check_by_level() {
        let set = LevelBasedPermissionSet::for_level(PermissionLevel::Gamemasters);
        assert!(set.has_permission(&Permission::HasCommandLevel(PermissionLevel::Moderators)));
        assert!(set.has_permission(&Permission::HasCommandLevel(PermissionLevel::Gamemasters)));
        assert!(!set.has_permission(&Permission::HasCommandLevel(PermissionLevel::Admins)));
    }

    /// Vanilla's one special-cased atom, both spellings.
    #[test]
    fn vanilla_level_set_special_cases_entity_selectors_at_gamemaster() {
        let gm = LevelBasedPermissionSet::for_level(PermissionLevel::Gamemasters);
        let mod_ = LevelBasedPermissionSet::for_level(PermissionLevel::Moderators);
        for node in [
            "commands/entity_selectors",
            "minecraft:commands/entity_selectors",
        ] {
            assert!(gm.has_permission(&Permission::atom(node)), "gm {node}");
            assert!(!mod_.has_permission(&Permission::atom(node)), "mod {node}");
        }
    }

    /// **The divergence, pinned.** Vanilla's level-based set denies an
    /// undeclared atom even to an owner; Bukkit's order (what [`Permissions`]
    /// implements) grants it to any op. Both behaviours are correct for their
    /// own upstream, and this test exists so nobody "fixes" one into the
    /// other without reading the module doc.
    #[test]
    fn vanilla_level_set_denies_an_undeclared_atom_where_bukkit_grants_it() {
        let owner_set = LevelBasedPermissionSet::for_level(PermissionLevel::Owners);
        assert!(
            !owner_set.has_permission(&Permission::atom("myplugin.admin")),
            "vanilla: every atom but entity_selectors is false at every level"
        );

        let mut permissions = Permissions::new();
        permissions
            .store
            .set_level(player_id(), PermissionLevel::Owners);
        assert!(
            permissions.has(player(), "myplugin.admin"),
            "Bukkit: an undeclared atom defaults to OP"
        );
    }

    /// `union` keeps the higher level.
    #[test]
    fn vanilla_level_set_union_keeps_the_higher_level() {
        let a = LevelBasedPermissionSet::for_level(PermissionLevel::Moderators);
        let b = LevelBasedPermissionSet::for_level(PermissionLevel::Admins);
        assert_eq!(a.union(b).level, PermissionLevel::Admins);
        assert_eq!(b.union(a).level, PermissionLevel::Admins);
    }

    // ---------------------------------------------------------------------
    // GrantSet text form
    // ---------------------------------------------------------------------

    /// LuckPerms' `-node` text form parses to a deny.
    #[test]
    fn grant_set_parses_the_luckperms_minus_prefix_as_a_deny() {
        let set = GrantSet::parse(["myplugin.*", "-myplugin.admin"]);
        let entries: Vec<_> = set.iter().collect();
        assert_eq!(
            entries,
            vec![
                ("myplugin.*", Grant::Allow),
                ("myplugin.admin", Grant::Deny),
            ]
        );
    }

    /// An empty store resolves nothing, so `best_match` really does return
    /// `None` rather than a default-shaped `Allow` — the precondition every
    /// step-5/6 test above depends on.
    #[test]
    fn an_empty_grant_set_matches_nothing() {
        let set = GrantSet::new();
        assert!(set.is_empty());
        assert!(set.best_match("anything", true).is_none());
    }
}
