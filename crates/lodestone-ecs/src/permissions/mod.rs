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
//!   26.2 jar's own permissions package: it has a five-variant command-level
//!   enum (`ALL`=0, `MODERATORS`=1,
//!   `GAMEMASTERS`=2, `ADMINS`=3, `OWNERS`=4) with an "is equal or higher
//!   than" comparison;
//!   a permission is either a plain named atom or "requires at least this
//!   command level"; a permission-set interface answers a single
//!   yes/no membership query, with an always-empty set, an always-full set,
//!   and a union operation; and a permission check is either
//!   "always passes" or "requires holding a given permission".
//!   [`PermissionLevel`] and [`Permission`] here are
//!   that model, transliterated in name and numbering so a future
//!   `ops.json`/`ClientboundCommandsPacket` consumer needs no mapping table.
//!
//! - **Bukkit/Paper** is what a *plugin author* expects, and it is a different
//!   shape: dotted string nodes, four-valued defaults, and attachments. Its
//!   resolution order is layered on top of the vanilla model rather than
//!   replacing it, exactly as the real Bukkit does (a subject's permissible
//!   base sits on
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
//! 5. **The node's declared default**, evaluated against op status — the
//!    node's default resolved against whether the subject is an operator,
//!    whose four values
//!    are exactly [`PermissionDefault`]'s: `TRUE`→`true`, `FALSE`→`false`,
//!    `OP`→`op`, `NOT_OP`→`!op`. A three-value description of the default —
//!    "`true`/`false`/`op`" — misses one: Bukkit has **four**, and
//!    `NOT_OP` is load-bearing for real plugins (it is how you gate a thing
//!    *away* from staff). See [`PermissionDefault`].
//!
//! 6. **An undeclared node falls back to [`DEFAULT_PERMISSION`], which is
//!    `Op`** — not `False`. This is the step most likely to surprise: Bukkit's
//!    own global fallback for an undeclared permission really is its
//!    op-default value, so in
//!    Bukkit a node no plugin ever declared is held by every operator. We
//!    match it, because a plugin ported from Bukkit will have been written
//!    against it. [`Permissions::strict`] flips this one step to `False` for a
//!    deployment that wants deny-by-default; nothing else changes.
//!
//! Steps 1 and 5–6 reproduce Bukkit's own permission-check algorithm: check
//! whether the permission was explicitly set for this subject and use that
//! value; otherwise, look up the permission's registered default and
//! evaluate it against op status; otherwise (the permission was never
//! registered by any plugin), evaluate the global fallback default against op
//! status.
//!
//! Note the case-folding: Bukkit permission nodes are **case-insensitive**,
//! and so are these — every node is normalised through
//! [`normalize_node`] on both the grant and the query side.
//!
//! ## Where we knowingly diverge from Bukkit, and why
//!
//! **Bukkit does not match wildcards at check time at all.** Its own
//! permission-check algorithm is an exact hash-map lookup;
//! `myplugin.*` only works in Bukkit because a plugin *declares* that
//! permission along with its child permissions, and those get flattened
//! into the attachment map when the permission is **set**. That
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
//! defaulted true)" would be.** Vanilla 26.2's own level-based permission-set
//! check does *not* do that — an
//! `Atom` permission returns `false` for **every** level except the one
//! hardcoded case `COMMANDS_ENTITY_SELECTORS` (which requires `GAMEMASTERS`):
//!
//! ```text
//! if the permission carries a required command level:
//!    return this level >= that required level
//! else:
//!    return this level >= GAMEMASTERS, if the permission is
//!       COMMANDS_ENTITY_SELECTORS, else false
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


mod grants;
mod store;
mod resolver;

pub use grants::{
    DEFAULT_PERMISSION, Grant, GrantSet, Permission, PermissionDefault, PermissionLevel,
    normalize_node,
};
pub use resolver::{
    LevelBasedPermissionSet, PermissionQuery, PermissionRegistry, PermissionResolver, Permissions,
};
pub use store::{Group, PermissionStore, PermissionSubject, SubjectPermissions};

#[cfg(test)]
mod tests;

