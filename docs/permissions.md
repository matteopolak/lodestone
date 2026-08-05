# Permissions

## What it is

The permission system plugins ask questions of: dotted nodes (`myplugin.admin.reload`),
wildcards (`myplugin.*`), per-node defaults, per-player and per-group grants with negation,
vanilla's five command levels, and a resolver trait a permissions *plugin* can use to take
the whole decision over. One resource, `lodestone_ecs::permissions::Permissions`, answering
one question: does this subject hold this permission?

Closes issues #125 (nodes, wildcards, defaults, group resolution) and #127 (op-level and
per-player/group resolution). Lives at `crates/lodestone-ecs/src/permissions.rs`.

```rust
use lodestone_ecs::permissions::{PermissionDefault, PermissionSubject, Permissions};

let mut permissions = Permissions::new();
permissions.declare("myplugin.admin", PermissionDefault::Op);
permissions.grant(player_uuid, "myplugin.*");
permissions.deny(player_uuid, "myplugin.admin.dangerous");

permissions.has(PermissionSubject::Player(player_uuid), "myplugin.admin.reload"); // true
permissions.has(PermissionSubject::Player(player_uuid), "myplugin.admin.dangerous"); // false
```

## How it works

### Two parity targets, layered

This subsystem answers to two upstreams at once, and the layering is the design:

- **Vanilla 26.2** has a real permission system — not the bare numeric op level issue #127
  describes. From `.cache/mc/26.2/src/net/minecraft/server/permissions/`:
  `PermissionLevel` is a five-variant enum (`ALL`=0 … `OWNERS`=4) with `isEqualOrHigherThan`;
  `Permission` is `Atom(Identifier) | HasCommandLevel(PermissionLevel)`; `PermissionSet` is
  `hasPermission(Permission) -> boolean` with `NO_PERMISSIONS`/`ALL_PERMISSIONS`/`union`;
  `PermissionCheck` is `AlwaysPass | Require`. `PermissionLevel` and `Permission` here are
  that model, transliterated in name and numbering, so an `ops.json` loader needs no mapping
  table.
- **Bukkit/Paper** is what a plugin author expects: dotted strings, four-valued defaults,
  attachments. It sits *on top of* the vanilla model, exactly as real Bukkit's
  `PermissibleBase` sits on top of vanilla's op level.

### The resolution order

`Permissions::check` resolves in this order. The module doc is the specification; this is a
summary.

| # | Step | Source |
|---|---|---|
| 1 | An installed `PermissionResolver` returning `Some` wins outright | #125's LuckPerms seam |
| 2 | The most specific matching grant (exact > `a.b.*` > `a.*` > `*`) | LuckPerms specificity |
| 3 | At equal specificity, the subject's **own** grant beats an inherited group grant | LuckPerms user-over-group |
| 4 | Within the same tier, a **deny** beats an allow | LuckPerms negation |
| 5 | The node's declared `PermissionDefault`, against op status | Bukkit `PermissionDefault.getValue(boolean op)` |
| 6 | An **undeclared** node falls back to `DEFAULT_PERMISSION`, which is `Op` | Bukkit `Permission.DEFAULT_PERMISSION` |

Steps 1 and 5–6 are Bukkit's `PermissibleBase.hasPermission` verbatim:

```java
String name = inName.toLowerCase();
if (isPermissionSet(name)) {
    return permissions.get(name).getValue();
} else {
    Permission perm = Bukkit.getServer().getPluginManager().getPermission(name);
    if (perm != null) {
        return perm.getDefault().getValue(isOp());
    } else {
        return Permission.DEFAULT_PERMISSION.getValue(isOp());
    }
}
```

Note the `toLowerCase()`: nodes are case-insensitive, normalised through `normalize_node` on
**both** the grant and the query side.

### Levels

`PermissionLevel` is vanilla's five, with `by_id` reproducing vanilla's CLAMP out-of-bounds
strategy (`ByIdMap.continuous(…, OutOfBoundsStrategy.CLAMP)`) — a hand-edited `ops.json` with
`"level": 99` really does mean `OWNERS`. `is_op()` is `>= MODERATORS`, because `ops.json`
cannot record a non-op: being in the file and being at least level 1 are the same condition.

## How to change it, and the gotchas

**The five things most likely to bite:**

1. **An undeclared node is held by every operator, not denied to everyone.** Bukkit's
   `Permission.DEFAULT_PERMISSION` really is `OP`. This is step 6 and it is the single most
   surprising step; `an_undeclared_node_is_held_by_ops_and_nobody_else` pins it. If you want
   deny-by-default, use `Permissions::strict()`, which changes **only** step 6.

2. **An exact deny does not cover its children.** `-myplugin.admin` leaves
   `myplugin.admin.reload` still allowed by a broader `myplugin.*`. Carving out a whole
   branch needs `-myplugin.admin.*`. This is LuckPerms' behaviour and the mistake most
   permissions configs make once.

3. **The order of steps 2–4 is not interchangeable, and one wrong ordering is silent.** An
   earlier draft compared specificity → negation → tier. Step "tier" could then only fire
   when specificity *and* direction already matched, in which case the resolved boolean is
   identical either way: the step was documented, implemented, and **could not change any
   answer**. Each of the three boundaries now has its own test:
   `group_exact_grant_beats_player_wildcard_grant`,
   `player_grant_beats_group_grant_at_equal_specificity`,
   `a_deny_beats_an_allow_at_equal_specificity`.

4. **Specificity is compared before tier**, so a group's exact `a.b` allow beats the
   player's own `a.*` deny. Deliberate, matches LuckPerms, and surprising.

5. **Wildcards are matched at *check* time, which bare Bukkit does not do.**
   `PermissibleBase.hasPermission` is an exact map lookup; `myplugin.*` only works in Bukkit
   because a declared permission's `getChildren()` is flattened into the attachment map when
   set. That cannot answer #125's "wildcard suffix matching" for an undeclared node at all,
   so we resolve wildcards LuckPerms-style. Consequence: a wildcard grant here matches nodes
   no plugin declared.

**Structural notes:**

- **Group inheritance is depth-first with a visited set** (`PermissionStore::collect_grants`),
  so a cyclic group graph terminates. `cyclic_group_inheritance_terminates` is the guard; the
  visited set is not an optimisation.
- **Specificity is a `u32` score, not a comparison function.** If you add a grant shape,
  give it a score in the same space and extend `grant_matches`. A second comparison path is
  how two callers start disagreeing about which grant wins.
- **`Permissions` denies atoms differently from vanilla, on purpose.** Vanilla's
  `LevelBasedPermissionSet.hasPermission` returns `false` for **every** atom at **every**
  level except the hardcoded `commands/entity_selectors` (which needs `GAMEMASTERS`). We
  follow Bukkit instead, because the consumer is a plugin API. Vanilla's exact behaviour is
  available as `LevelBasedPermissionSet` for a caller that wants host parity, and
  `vanilla_level_set_denies_an_undeclared_atom_where_bukkit_grants_it` pins the two apart so
  neither gets "fixed" into the other.
- **`LevelBasedPermissionSet::union` knowingly does not match the jar.** 26.2's returns the
  *lower*-level set when `this` is higher, contradicting `PermissionSetUnion`'s own OR
  semantics. We keep the higher level. Documented in place.
- **Nothing here touches the network.** No protocol family carries an op level
  (`AbilitiesChanged` has six fields, none of them a level), so `PermissionStore::set_level`
  is the only way a level is ever set today. A future `ops.json` loader is its caller.

## Configuration

None — no env vars, no files. `Permissions::default()` is an empty registry with no grants
and no resolver, which by step 6 means *ops hold every node and nobody else holds any*:
vanilla's op/non-op split, which is #125's "usable with zero permission plugins installed".

`PluginCommandsPlugin` inserts the resource; a driver that wants permissions without commands
inserts `Permissions` itself.

## Dependencies

- `bevy_ecs` for `Resource`, `uuid` for the subject id. Nothing else.
- Deliberately **not** `lodestone-command`: a permission is a string to this module. The join
  between permissions and the command tree is the *caller's*, in
  `lodestone_ecs::commands` — see [plugin-commands.md](plugin-commands.md).

## Related

- [plugin-commands.md](plugin-commands.md) — the first consumer, and #122's per-node gating.
- [roadmap/plugin-framework.md](roadmap/plugin-framework.md) — the capability audit that
  named #125 as the blocking substrate.
