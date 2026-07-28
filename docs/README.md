# Lodestone docs

Per-feature documentation. See also the root [`DESIGN.md`](../DESIGN.md)
(architecture and rationale) and [`HANDOFF.md`](../HANDOFF.md) (deferred work).

- [Fluid classification](./fluid-classification.md) — the one answer to "does this
  block state carry water?", shared by the mesher and by physics (swimming, fog,
  overlay, ambient sounds).
- [Item GUI geometry](./item-gui-geometry.md) — baking block items into 3-D
  inventory-slot geometry, and the pose/projection matrices that place them.
- [Entity rendering](./entity-rendering.md) — how an entity type resolves to a
  mesh, a texture and a `setupAnim`, and the two places that resolution has
  silently picked the wrong mob.
- [Crafting](./crafting.md) — the recipe data model and matching rules, loading
  the vanilla corpus from the client jar's datapack JSON, and the crafting-table
  menu layout (including who actually computes the result slot).
