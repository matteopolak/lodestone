//! `Sim`'s interaction/combat cluster, split out of `sim.rs` into its own
//! module: `break_block`, `begin_attack`/`begin_attack_demo`/
//! `begin_attack_live`, `entity_target`, `attack_entity`,
//! `maybe_spawn_crit_particles`, `interact_entity`, `end_attack`, `use_item`,
//! `end_use`/`end_use_live`, `use_item_live`, `use_item_generic`,
//! `predict_block` and `place_block` — seam 3 of the sim.rs decomposition
//! sequence (seam 1 was the test module, `sim/tests.rs`; seam 2 was placement
//! prediction, `sim/placement.rs`). This is `impl Sim` methods, not free
//! functions, so nothing needed re-exporting: a method call resolves through
//! the `Sim` type regardless of which file defines it, and `sim::actions` is
//! a *descendant* of `sim`, so it already has the same visibility into
//! `Sim`'s private fields and `sim.rs`'s other private items that
//! `sim::tests` has always had — the same reasoning that module's own move
//! relied on.
//!
//! `use super::*;` mirrors `sim/tests.rs`'s own top line for exactly that
//! reason: it pulls in `Sim`'s private fields' types, sim.rs's other private
//! helpers (`face_from_normal`, `hit_cursor`, …) and everything `sim.rs`
//! itself re-exports from `sim::placement`, with no need to enumerate them.
//!
//! **`placement_facts` and `block_intersects_player` left this `impl Sim`
//! block entirely**, later, for `PlaceIntent` (`docs/plugin-api.md`) — moved
//! to free functions in `sim/placement.rs`, parameterised over the two reads
//! that used to come from `self`, since `crate::interact::drive_placement` (a
//! `GameTick` system) needs the identical resolution with no `Sim` to call a
//! method on. `use_item_live`/`place_block` here call the free functions now.

use super::*;
// That fix's veto registry -- see `attack_entity`.
use lodestone_ecs::player::{FireworkBoost, ItemUseTicks};
use lodestone_ecs::veto::{ActionVetoes, VerbContext, Verdict};
use lodestone_physics::UseEffects;

/// `TridentItem.THROW_THRESHOLD_TIME` — how long the use
/// button must be held before a release does anything at all. That fix.
const RIPTIDE_MIN_HELD_TICKS: u32 = 10;

/// The deterministic part of `FireworkRocketEntity`'s lifetime for a standard
/// 1-gunpowder rocket: `10 * flightCount` with `flightCount = 1 +
/// flightDuration = 2`. See
/// [`Sim::start_firework_boost_if_gliding`] for the two random terms this
/// deliberately omits and why. That fix.
const FIREWORK_BOOST_TICKS: u32 = 20;

/// `minecraft:riptide`'s holder id in the synced `minecraft:enchantment`
/// registry, derived from the alphabetical position of
/// `data/minecraft/enchantment/riptide.json` among 26.2's 43 built-in
/// enchantments (33rd, so id `32`). That fix.
///
/// **No longer the primary resolution** — [`Sim::riptide_enchantment_id`] asks
/// the server's own `minecraft:enchantment` registry order first. This is the
/// fallback for the window before that table arrives, and it is kept rather than
/// deleted because "no table yet" and "this server has no riptide" are different
/// answers and only one of them should read as level 0.
const RIPTIDE_ENCHANTMENT_ID: i32 = 32;

/// Whether `id`'s vanilla use-animation is anything other than "none" — i.e.
/// whether right-clicking with it would actually enter vanilla's
/// `isUsingItem()` state at all, the gate [`Sim::use_item_live`] applies
/// before arming [`UsingItem`]/[`ItemUseEffects`].
///
/// Vanilla's own item base resolves this from three
/// components: `minecraft:consumable` (food/drink — [`consumable_for_item`]),
/// `minecraft:blocks_attacks` (the shield) and `minecraft:kinetic_weapon`
/// (the seven `*_spear` items, built with a dedicated spear-properties
/// helper); everything else
/// falls to "none". Seven specific items override it unconditionally, each
/// returning one fixed use-animation regardless of stack state:
/// the bow (`minecraft:bow`), the crossbow (`minecraft:crossbow`),
/// the spyglass (`minecraft:spyglass`), the trident
/// (`minecraft:trident`), the brush (`minecraft:brush`), the instrument item
/// (`minecraft:goat_horn` is the only item built on it) and the bundle
/// (every `*_bundle` colour, plus the plain `minecraft:bundle`, share the
/// same base item).
///
/// A sword, a tool, a plain block and an empty hand all have none of the
/// three components and no such override, so the animation lookup returns
/// "none" and vanilla's `use()` never enters the item-use state for them — this
/// must return `false` for exactly that set.
#[must_use]
fn item_has_use_animation(id: &str) -> bool {
    if lodestone_game::consumable::consumable_for_item(id).is_some() {
        return true;
    }
    let path = id.rsplit_once(':').map_or(id, |(_, path)| path);
    matches!(
        path,
        "shield" | "bow" | "crossbow" | "spyglass" | "trident" | "brush" | "goat_horn" | "bundle"
    ) || path.ends_with("_spear")
        || path.ends_with("_bundle")
}

/// Whether vanilla's `Item.use` can actually enter an item-use state for the
/// held item and current player state. Food is the one use-animation family
/// whose `use()` can return `FAIL`: `Player.canEat` refuses ordinary food when
/// the hunger bar is full. Keep this gate beside `item_has_use_animation`, but
/// feed it the same server-reported vitals used by the consume animation so
/// the press edge cannot arm movement slowdown for a use the server will reject.
#[must_use]
fn item_can_start_use(id: &str, food_level: Option<i32>, invulnerable: bool) -> bool {
    item_has_use_animation(id)
        && lodestone_game::food::always_eat_for_food(id).is_none_or(|always_eat| {
            food_level.is_none_or(|level| {
                lodestone_game::food::can_eat(always_eat, level, invulnerable)
            })
        })
}

/// Strip an item id's namespace, so the two swing tables below can be written
/// against bare paths the way [`item_has_use_animation`] already is.
fn item_path(id: &str) -> &str {
    id.rsplit_once(':').map_or(id, |(_, path)| path)
}

/// Whether a **generic use** of `id` — vanilla's own generic item-use path,
/// reached from its per-hand use fallback — produces a success result whose
/// swing source is the client, and so is
/// one of the uses that swings the arm.
///
/// This is the table the "every right-click swings" report was about. Vanilla
/// does **not** swing on a generic use in general: a "consume" result carries
/// no client-side swing, and vanilla's own item base's `use()` returns "consume" for
/// a consumable, for `minecraft:blocks_attacks` (the shield) and for
/// `minecraft:kinetic_weapon`, "pass" for everything else — a sword, a
/// pickaxe, a plain block held at open air. The bow, the crossbow,
/// the trident, the instrument item and the spyglass all return "consume"
/// too. So drawing a bow, raising a shield, eating and aiming a spyglass are
/// all *silent* in vanilla and were all swinging here.
///
/// What is left is the set of `use()` overrides that really do return an
/// unqualified success. Two of vanilla's items return a server-attributed
/// success instead (the item on a stick base, and the ender eye) — that
/// carries no client-side swing either, so the client does not swing for those either, and
/// they are deliberately absent below.
///
/// `equippable`'s branch of the base `use()` is the one remaining plain
/// success and is **not** here: it depends on what is already in the armour slot, so
/// [`Sim::predict_equip_swap`] answers it from the live menu instead.
///
/// # The over-approximation, named
///
/// Five of these return success only for a hit the client would have to
/// re-cast to know about — the boat, the bucket, the glass bottle and
/// the spawn egg each run their own fluid-aware hit-result ray,
/// which this shell does not model (its block ray is a separate, non-fluid
/// one), and all four fall back to "pass" on a miss. They are listed as swinging,
/// because the click a player makes with a bucket or a boat in hand is
/// overwhelmingly the one that lands on water. **The case this gets wrong is
/// a bucket, boat, glass bottle or spawn egg right-clicked at open sky**,
/// where vanilla returns "pass" and stays still and this swings.
///
/// `firework_rocket` is *not* in that class: the firework rocket's own use
/// path gates on whether the player is fall-flying, which this client tracks, so the caller passes
/// `fall_flying` and the answer is exact.
#[must_use]
fn generic_use_swings(id: &str, fall_flying: bool) -> bool {
    let path = item_path(id);
    if path == "firework_rocket" {
        return fall_flying;
    }
    if matches!(
        path,
        // Unconditional `SUCCESS` in the jar.
        "snowball"
            | "egg"
            | "ender_pearl"
            | "wind_charge"
            | "experience_bottle"
            | "fishing_rod"
            | "written_book"
            | "writable_book"
            | "knowledge_book"
            | "map"
            | "splash_potion"
            | "lingering_potion"
            | "bundle"
            // The over-approximated fluid-ray four.
            | "bucket"
            | "glass_bottle"
    ) {
        return true;
    }
    path.ends_with("_bundle")
        || path.ends_with("_spawn_egg")
        || path.ends_with("_boat")
        || path.ends_with("_raft")
        // `milk_bucket` is a consumable, not a fluid-bucket item: its use is the
        // base item's "consume" arm, which does not swing.
        || (path.ends_with("_bucket") && path != "milk_bucket")
}

/// Whether `id`'s **use-on-block** path — vanilla's `case BLOCK` arm, reached
/// through its own client-side use-on-block dispatch
/// — returns a success that swings.
///
/// Needed because [`Placement::use_on`] answers a narrower question than
/// vanilla's own use-on-block dispatch does. Its [`UseOnDecision::Interact`] and
/// [`UseOnDecision::Place`] cover the block actuating and a block item
/// placing, both success; its [`UseOnDecision::Nothing`] collapses two
/// vanilla outcomes that swing differently — the base item's "pass" use-on
/// (a sword or a pickaxe against stone: no swing, fall through to the generic
/// use) and the overrides that light, till, strip, shear, wax or place an
/// entity against that block (success: swing, and vanilla *returns* rather
/// than falling through).
///
/// The over-approximation is the same shape as [`generic_use_swings`]'s and
/// it is one-sided the same way: each of these is success only against the
/// block it acts on — a hoe on tillable dirt, an axe on strippable log,
/// honeycomb on unwaxed copper — and this shell does not carry the tags to
/// test that. **The case this gets wrong is one of these items right-clicked
/// against a block it cannot act on**, where vanilla is "pass" and silent.
///
/// Four narrow use-on-block overrides are deliberately left out, because for them
/// the success arm is the rare one and listing them would swing on the
/// common "pass": the compass (lodestone only), the map (lectern only),
/// the ender eye (end portal frame only) and the potion (a *water* bottle
/// on a mud-convertible block only).
#[must_use]
fn use_on_block_swings(id: &str) -> bool {
    let path = item_path(id);
    if matches!(
        path,
        "flint_and_steel"
            | "fire_charge"
            | "bone_meal"
            | "shears"
            | "honeycomb"
            | "end_crystal"
            | "armor_stand"
            | "item_frame"
            | "glow_item_frame"
            | "painting"
            | "minecart"
            | "debug_stick"
            | "firework_rocket"
    ) {
        return true;
    }
    path.ends_with("_axe")
        || path.ends_with("_hoe")
        || path.ends_with("_shovel")
        || path.ends_with("_minecart")
        || path.ends_with("_spawn_egg")
}

impl Sim {
    /// Break the currently targeted block (set it to air) and remesh. Returns
    /// whether a block was broken.
    ///
    /// This is the **demo-world** direct edit: it mutates the shell's offline
    /// world in place. On a live server the shell must instead route the dig
    /// through the server (see [`begin_attack`](Self::begin_attack)), or the
    /// break would be local-only and the server would restore the block on the
    /// next chunk update.
    pub fn break_block(&mut self) -> bool {
        let Some(hit) = self.target() else {
            return false;
        };
        // Read the state *before* clearing the cell: the debris takes its
        // texture from the block that broke, and after `set_block_world` the
        // cell is air and that information is gone.
        let broken = self.block_at_world(hit.block);
        if self.set_block_world(hit.block, id::AIR) {
            // The demo world has no `ActionQueue` swing to piggy-back on (see
            // `drain_action_queue`), so the animation is started here. Without
            // this the offline demo — including every headless scene — could not
            // exercise the swing at all, which is the one world structurally
            // guaranteed not to.
            self.swing_hand();
            // Full-cube shape: vanilla derives the fragment grid from the
            // block's outline shape, which the shell does not carry, so debris
            // from a slab or fence fills the whole cell rather than hugging the
            // model.
            self.particles_mut(|p| p.destroy_block(hit.block, broken, [1.0; 3]));
            // Vanilla's own break is *predicted*, not received: the client's
            // own destroy-block path runs a will-destroy hook, then spawns the
            // destroy particles, then raises a level event, and
            // the client-side level dispatch ignores the exclusion and dispatches
            // straight into its own level-event handling locally
            // — sound and debris together. This is
            // the offline mirror of that; the live predicted break is still
            // silent because its emit lives in `interact.rs`'s ECS system, which
            // has no audio handle (see `docs/sound-playback.md`).
            self.play_block_break_sound(hit.block, broken);
            self.remesh_around(hit.block);
            self.set_target(None);
            true
        } else {
            false
        }
    }

    /// Begin an attack (left-click / attack button pressed).
    ///
    /// Vanilla's `Minecraft.startAttack` switches
    /// on `hitResult.getType()` and swings the arm **unconditionally after the
    /// switch**, on every arm of it, miss included:
    ///
    /// * `ENTITY` — `this.gameMode.attack(player, entity)`, i.e. send the
    ///   attack.
    /// * `BLOCK`, and the block is *not* air — `startDestroyBlock`, i.e. begin
    ///   mining. (Vanilla deliberately **falls through** to `MISS` when the
    ///   block at `hitResult`'s position is air; this shell's `target()`
    ///   never reports a hit on an air cell in the first place — the ray only
    ///   stops at a *solid* cell — so that fallthrough has no case to cover
    ///   here.)
    /// * `MISS` (or no target at all) — nothing happens server-side, but the
    ///   arm still swings.
    ///
    /// Before an earlier fix, only the `BLOCK`-with-a-dig-that-actually-starts
    /// arm ever reached [`Self::swing_hand`] (through `drive_mining`'s own
    /// queued `SwingArm`, see `drain_action_queue`'s docs) — so punching air,
    /// an entity, or empty space produced no *local* animation at all. This
    /// method is the one place all three branches now funnel through.
    ///
    /// That fix only reached [`Self::swing_hand`], which is purely the local
    /// animation clock — it does not touch the wire. On a live server the
    /// `ENTITY` and `MISS` arms called it **directly** rather than through
    /// [`Self::swing_main_hand_live`], so this client's own arm swung while no
    /// `ClientAction::SwingArm` ever reached [`ActionQueue`] or the socket:
    /// the local player saw their own swing and every other client saw
    /// nothing, exactly the reported symptom ("if i punch the air, it doesnt
    /// send the arm swing packet so other players dont see it"). The `BLOCK`
    /// arm was never affected — `drive_mining` queues its own `SwingArm`
    /// through `ActionQueue` the instant a dig starts, which is why hitting a
    /// block already worked.
    ///
    /// `case ENTITY` takes priority over `case BLOCK`: [`EntityRayTarget`] is
    /// already the nearer of an entity-or-block pick (see
    /// [`Self::update_entity_target`]'s docs), so a `Some` there means mining
    /// must not start on this click even when [`RayTarget`] also holds a
    /// block.
    ///
    /// # What is deliberately not modelled here
    ///
    /// Vanilla's `attackStrengthTicker`/`getAttackStrengthScale` cooldown, the
    /// crit condition and the sweep-attack condition are real per-hit vanilla
    /// mechanics, but every one of them exists only to scale **local** sound/
    /// particle feedback and the crosshair cooldown indicator — the damage
    /// number itself is server-authoritative (the wire `Attack` packet
    /// carries only the target id, no damage or strength scalar; see
    /// `EntityInteraction::Attack`'s encoding in
    /// `crates/protocol/v770/src/adapter.rs`). None of those consumers exist
    /// in this shell yet: the crosshair indicator is `hud.rs`'s (held by
    /// another agent), and sweep/crit sound-and-particle feedback is
    /// `entities.rs`/asset work, also out of this file's scope. Building a
    /// ticker nothing reads would be exactly the unconsumed-island class
    /// `CLAUDE.md`'s core rule warns about, so it stays unbuilt rather than
    /// built and orphaned — whoever adds the crosshair pip or the sweep sound
    /// is the right owner for it, alongside the half it feeds.
    pub fn begin_attack(&mut self) {
        if self.is_live() {
            self.begin_attack_live();
        } else {
            self.begin_attack_demo();
        }
    }

    /// The demo-world half of [`Self::begin_attack`]: break the targeted
    /// block if there is one ([`Self::break_block`] already swings on
    /// success), or swing on a miss — the offline mirror of vanilla's
    /// unconditional swing. The demo ECS holds no networked entities (see
    /// [`Self::update_entity_target`]'s docs), so there is no `case ENTITY` to
    /// take here; only `BLOCK` vs `MISS`.
    fn begin_attack_demo(&mut self) {
        if !self.break_block() {
            self.swing_hand();
        }
    }

    /// The live half of [`Self::begin_attack`]. See that method's docs for the
    /// three-way switch this implements.
    ///
    /// Two more gates now sit ahead of that switch, both ported from
    /// vanilla's own attack-start entry point in the same order the jar checks them:
    ///
    /// 1. **Spectator.** Vanilla's own is-spectator check, checked before
    ///    any item or hit-result logic at all — see [`Self::spectate_or_no_action`].
    ///    Neither of vanilla's two spectator arms swings the arm or falls
    ///    through to the ordinary switch below.
    /// 2. **Piercing weapon.** The held item's own piercing-weapon data component,
    ///    checked next and, when present, taken unconditionally regardless of
    ///    what — if anything — is under the crosshair; vanilla's own switch on
    ///    `hitResult.getType()` is never reached for a piercing weapon. See
    ///    [`Self::held_item_is_piercing_weapon`]/[`Self::stab`].
    pub(crate) fn begin_attack_live(&mut self) {
        if self.is_dead() {
            return;
        }
        if self.is_spectator() {
            self.spectate_or_no_action();
            return;
        }
        if self.held_item_is_piercing_weapon() {
            self.stab();
            self.swing_main_hand_live();
            return;
        }
        if let Some(entity_id) = self.entity_target() {
            self.attack_entity(entity_id);
            self.swing_main_hand_live();
            return;
        }
        if self.target().is_some() {
            // Unchanged from before this fix: arms the hold-to-mine loop.
            // `drive_mining` itself queues the `SwingArm` the instant a dig
            // actually starts, through the same `ActionQueue`/
            // `drain_action_queue` funnel every other tick-driven swing uses.
            self.write(|w| w.resource_mut::<Attacking>().0 = true);
            return;
        }
        // MISS: no block, no entity. Vanilla still swings.
        self.swing_main_hand_live();
    }

    /// Vanilla's own attack-start entry point's spectator branch
    /// (vanilla's own client-side spectate/no-op-spectate pair): left-clicking an
    /// entity while spectating attaches the spectator camera to it
    /// (`SpectatorAction { target_entity_id: Some(id) }`); left-clicking
    /// anything else — a block, or nothing at all — detaches it
    /// (`target_entity_id: None`). [`Self::entity_target`] already resolves
    /// the same nearer entity-or-block pick every other left-click branch
    /// here uses, so both vanilla arms fold into one send. Neither vanilla
    /// arm swings the arm or falls through to the ordinary attack switch —
    /// vanilla's own attack-start entry point returns immediately after either call.
    fn spectate_or_no_action(&mut self) {
        let target_entity_id = self.entity_target();
        if let Some(net) = &self.net {
            net.send_action(ClientAction::SpectatorAction { target_entity_id });
        }
    }

    /// Whether the main-hand stack carries `minecraft:piercing_weapon`
    /// (`DataComponents.PIERCING_WEAPON`) — `Minecraft.startAttack`'s gate,
    /// checked before the normal ENTITY/BLOCK/MISS switch. See
    /// [`lodestone_game::item::is_piercing_weapon`]'s own doc for why this
    /// checks item identity (the seven real spear items) rather than an
    /// actual component value.
    #[must_use]
    fn held_item_is_piercing_weapon(&self) -> bool {
        let slot = self.selected_slot();
        self.player_menu()
            .player_native(slot)
            .is_some_and(|stack| lodestone_game::item::is_piercing_weapon(stack.item()))
    }

    /// Vanilla's own client-side piercing-attack path's outbound half: send the wire
    /// `Stab` action. Vanilla's own local-only follow-up
    /// (an on-attack hook and a post-piercing-attack hook, a sound) all feed the crosshair
    /// cooldown indicator and hit sound/particles this shell does not model
    /// yet for the *ordinary* attack path either — see [`Self::begin_attack`]'s
    /// own doc for why that gap is out of scope here too, same reasoning.
    /// The caller is responsible for the swing, matching vanilla's own
    /// main-hand swing sitting beside
    /// the piercing-attack call at the call site rather than inside it.
    fn stab(&mut self) {
        if let Some(net) = &self.net {
            net.send_action(ClientAction::Stab);
        }
    }

    /// Swing the main hand for a **live** discrete click: the wire packet and
    /// the local animation together, one call so a caller cannot reach for
    /// the local-only half by mistake.
    ///
    /// [`Self::begin_attack_live`]'s `ENTITY` and `MISS` arms are discrete
    /// click events, not per-tick ones, so — same reasoning as
    /// [`Self::attack_entity`], [`Self::interact_entity`] and
    /// [`Self::end_attack`] — the send goes straight to the socket rather
    /// than through [`ActionQueue`], which only drains inside the tick loop.
    /// Mirrors [`Self::interact_entity`]'s own
    /// `net.send_action(SwingArm) then swing_hand()` pair exactly; that
    /// method already got this right; `begin_attack_live` did not.
    fn swing_main_hand_live(&mut self) {
        if let Some(net) = &self.net {
            net.send_action(ClientAction::SwingArm { hand: Hand::Main });
        }
        self.swing_hand();
    }

    /// The entity [`EntityRayTarget`] currently names, if any — the live
    /// left-click's attack target.
    #[must_use]
    pub fn entity_target(&self) -> Option<i32> {
        self.read(|w| w.resource::<EntityRayTarget>().0)
    }

    /// `key.pickItem` — vanilla's own pick-block-or-entity handling
    ///, middle-click by default
    ///. `include_data` is vanilla's own control-key-held check.
    ///
    /// Entity wins over block, for the same reason [`Self::begin_attack_live`]
    /// already gives: [`EntityRayTarget`] is resolved as the *nearer* pick, so
    /// preferring it here matches what the crosshair is actually on rather than
    /// re-deciding the priority.
    ///
    /// Two distinct actions rather than one with an enum, because 26.2 splits
    /// them on the wire — `PickItemFromBlock` carries a packed `BlockPos`,
    /// `PickItemFromEntity` a VarInt entity id (see the v770 adapter's own
    /// arms). Both encoders existed with **zero producers** before this method,
    /// the same outbound-island shape `ClientAction::SetFlying` was caught in.
    ///
    /// Sent directly rather than through [`ActionQueue`], like the attack and
    /// use paths: that queue drains inside the tick loop, and this is a discrete
    /// click, not a per-tick one. No game-mode gate — vanilla's pick works in
    /// every mode, spectator included.
    pub fn pick_block_or_entity(&mut self, include_data: bool) {
        if let Some(entity_id) = self.entity_target() {
            if let Some(net) = &self.net {
                net.send_action(ClientAction::PickItemFromEntity {
                    entity_id,
                    include_data,
                });
            }
            return;
        }
        let Some(hit) = self.target() else { return };
        let pos = BlockPos::new(hit.block[0], hit.block[1], hit.block[2]);
        if let Some(net) = &self.net {
            net.send_action(ClientAction::PickItemFromBlock { pos, include_data });
        }
    }

    /// Send the serverbound attack for `entity_id` — vanilla's own
    /// client-side attack path's outbound half. Lowers to
    /// `ClientAction::InteractEntity { interaction: EntityInteraction::Attack,
    /// .. }`, which the v770 adapter already encodes as the dedicated `Attack`
    /// packet (26.2 split entity-attack out of the old combined interact
    /// packet; see `crates/protocol/v770/src/adapter.rs`'s `InteractEntity`
    /// arm) — this method is the first caller that ever constructs the
    /// variant; the encoder was previously dead, unused code.
    ///
    /// Sent directly, like [`Self::use_item_live`]'s two sends, rather than
    /// queued through [`ActionQueue`]: that queue only drains inside the tick
    /// loop (see `crate::interact`'s "how to change it"), and an attack is a
    /// discrete click event, not a per-tick one.
    ///
    /// Also resets [`AttackStrengthTicker`] to `0` — vanilla's own
    /// client-side attack path resets its attack-strength ticker
    /// right after the client-side predicted attack call.
    /// Unconditional on every entity target, exactly like vanilla's call site:
    /// there is no client-side `cannotAttack` gate here (damage is fully
    /// server-authoritative per `docs/combat.md`), so every left-click on an
    /// entity restarts the cooldown regardless of whether the server ends up
    /// applying any damage.
    fn attack_entity(&mut self, entity_id: i32) {
        // That fix's entity-damage veto. This is one of the three verbs that
        // does NOT go through `ActionQueue` (it writes the socket directly, to
        // control wire order for a discrete click), so the outbound
        // `EgressFilters` hook cannot see it -- the veto has to be asked here.
        //
        // Read through `self.read`, and the predicate is handed only the
        // `VerbContext`: it must not re-enter the `World`, because we are
        // inside a read guard on it (`handle.rs`'s rule 1). That constraint is
        // why `ActionVetoes::allows` takes no `&World`.
        let vetoed = self.read(|w| {
            w.get_resource::<ActionVetoes>().is_some_and(|vetoes| {
                vetoes.allows(&VerbContext::EntityDamage {
                    target_entity_id: entity_id,
                }) == Verdict::Deny
            })
        });
        if vetoed {
            return;
        }
        // The same tick-driven intent `use_item_live` reads for its own
        // sneaking bit, so a sneak-attack cannot disagree with what the wire
        // already told the server this tick's crouch state is.
        let sneaking = self.movement_intent().sneak;
        let local = self.local;
        if let Some(net) = &self.net {
            net.send_action(ClientAction::InteractEntity {
                entity_id,
                interaction: EntityInteraction::Attack,
                sneaking,
            });
        }
        // Vanilla's own order (its own client-side attack path):
        // the packet, then the
        // client-side attack prediction — whose crit
        // condition reads `attackStrengthTicker` **before** it is reset — and
        // only then `resetAttackStrengthTicker()`. Reading the ticker after
        // zeroing it here would make `fullStrengthAttack` false on every
        // attack, including the one that just landed at full charge, so this
        // call must stay above the reset below.
        self.maybe_spawn_crit_particles(entity_id);
        self.write(|w| {
            if let Some(mut ticker) = w.get_mut::<AttackStrengthTicker>(local) {
                ticker.0 = 0;
            }
        });
    }

    /// Vanilla's local-only crit-particle prediction — vanilla's own attack
    /// path's crit flag is full-strength-attack anded with a can-crit check
    ///, whose visual half is
    /// its own visual-effects hook's crit call, which spawns a tracking
    /// particle emitter.
    ///
    /// # This is real vanilla dual simulation, not an approximation invented
    /// for this port
    ///
    /// Vanilla's own client-side attack path runs the **client's own copy** of
    /// the attack call independently
    /// of, and before, the server's authoritative copy of the same method —
    /// the server computes the real damage, the client predicts only the
    /// cosmetic trigger (sound + particle) so it does not wait a round trip to
    /// see feedback on its own swing. The wire `Attack` packet itself carries
    /// no damage or crit flag (`docs/combat.md`); nothing here affects what
    /// the server decides.
    ///
    /// # Condition, checked against the jar rather than assumed
    ///
    /// Vanilla's own can-crit check: positive fall distance, not on ground,
    /// not on a climbable, not in water, not mobility-restricted,
    /// not a passenger, the target is a living entity, and not sprinting.
    /// The full-strength-attack gate (attack-strength scale over a 0.5-tick
    /// partial exceeding `0.9`)
    /// is the caller's own gate, not part of
    /// the can-crit check — hence [`Self::attack_strength_scale_at`] rather
    /// than reusing [`Self::attack_strength_scale`]'s `a = 0.0`, which is a
    /// different call site's (the crosshair's) partial-tick argument.
    ///
    /// Two vanilla clauses are not modelled, and the divergence is small and
    /// explained rather than silent:
    /// - **`!onClimbable` is not read separately.** This engine resets
    ///   `fall_distance` to `0.0` the instant `tick_air` finds a climbable —
    ///   matching vanilla's own on-climbable handling, folded into `tick_air` per
    ///   [`lodestone_physics::player::PlayerState::fall_distance`]'s own
    ///   "Climbable reset" bullet — so `fall_distance > 0.0` already implies
    ///   not-on-climbable in this port's physics model. Checked against that
    ///   source rather than guessed.
    /// - **`!isMobilityRestricted`/`!isPassenger`, and the outer `baseDamage >
    ///   0.0F || magicBoost > 0.0F` gate, are not modelled.** This shell has
    ///   no riding state (`docs/combat.md`'s knockback section notes the same
    ///   absence for a different mechanic) and no local weapon-damage/
    ///   enchantment computation to derive `baseDamage`/`magicBoost` from —
    ///   the identical gap [`Self::attack_strength_delay`]'s own doc names for
    ///   `lodestone-data` carrying no per-item attack-speed census. The only
    ///   case this can diverge on is an attack that deals zero base damage
    ///   (an already-broken or damage-less item), which vanilla itself treats
    ///   as "nothing happens" at the outer `if` — the crit particle is cosmetic
    ///   and no damage number depends on it either way.
    ///
    /// # The particle burst: one tick of vanilla's own tracking emitter, not three
    ///
    /// Vanilla's own tracking emitter runs for **3 ticks**,
    /// spawning up to 16 candidates per tick (filtered to a unit sphere,
    /// ~52% pass) that track the entity's *current* position each tick. This
    /// shell's particle system has no per-attack persistent emitter — every
    /// existing local spawn ([`crate::particles::Particles::destroy_block`]/
    /// `breaking_block`) is a one-shot burst — so this spawns **one** tick's
    /// worth (16 candidates, same unit-sphere filter) at the target's
    /// position at the moment of the attack, rather than adding new
    /// multi-tick emitter machinery for a purely cosmetic burst. The
    /// per-candidate position/velocity formula (vanilla's own entity-position
    /// accessors) and the emitted particle's own physics
    /// (`lodestone_particle::emit::crit`) are both exact; only the tick count
    /// is a disclosed simplification.
    fn maybe_spawn_crit_particles(&mut self, entity_id: i32) {
        if self.attack_strength_scale_at(0.5) <= 0.9 {
            return;
        }
        let Some((feet, width, height)) = self.read(|w| {
            let target = w.resource::<EntityIndex>().get(entity_id)?;
            let pos = w.get::<Position>(target)?;
            let kind = w.get::<EntityKind>(target)?;
            let facts = w.resource::<VersionData>().entity_facts(&kind.0)?;
            let type_id = lodestone_data::entity_types::entity_type_id_parts(
                kind.0.namespace(),
                kind.0.path(),
            )?;
            lodestone_data::entity_census::is_living(type_id)
                .unwrap_or(false)
                .then_some((pos.0, facts.dimensions.width, facts.dimensions.height))
        }) else {
            return;
        };
        let local = self.local;
        let (fall_distance, on_ground) = self.read(|w| {
            w.get::<PhysicsState>(local)
                .map_or((0.0, true), |s| (s.0.fall_distance, s.0.on_ground))
        });
        if fall_distance <= 0.0 || on_ground {
            return;
        }
        if self.fluid_state().in_water() || self.movement_intent().sprint {
            return;
        }
        self.particles_mut(|p| {
            let engine = p.engine_mut();
            for _ in 0..16 {
                let xa = f64::from(engine.rng().next_f32()) * 2.0 - 1.0;
                let ya = f64::from(engine.rng().next_f32()) * 2.0 - 1.0;
                let za = f64::from(engine.rng().next_f32()) * 2.0 - 1.0;
                if xa * xa + ya * ya + za * za > 1.0 {
                    continue;
                }
                let x = f64::from(feet.x) + f64::from(width) * (xa / 4.0);
                let y = f64::from(feet.y) + f64::from(height) * (0.5 + ya / 4.0);
                let z = f64::from(feet.z) + f64::from(width) * (za / 4.0);
                particle_emit::crit(engine, x, y, z, xa, ya + 0.2, za);
            }
        });
    }

    /// Send the serverbound **use-on-entity** for `entity_id` — vanilla's own
    /// client-side interact path, the outbound half of mounting a boat,
    /// minecart or saddled animal.
    ///
    /// This is the mirror image of [`Self::attack_entity`]: same packet family,
    /// same direct-send reasoning (a click is a discrete event, not a per-tick
    /// one, and [`ActionQueue`] only drains inside the tick loop), same
    /// tick-derived `sneaking` bit so the local decision cannot disagree with the
    /// crouch state the wire already reported this tick. The differences are the
    /// interaction kind and that there is no attack cooldown to reset.
    ///
    /// **`Interact`, never `InteractAt`** — see [`Self::use_item_live`]'s entity
    /// branch for why the entity-local hit position is not fabricated here.
    ///
    /// # The swing here is unconditional, and that is a known divergence
    ///
    /// Vanilla is not: vanilla's own per-hand use-item dispatch's entity case swings only
    /// when its own game-mode interact call returns an unqualified success
    /// *and* that success's swing source is the client. Every other outcome
    /// — the overwhelmingly common "pass" from right-clicking a hostile mob
    /// with a sword — leaves the arm still.
    ///
    /// The client half of that decision is vanilla's own chain of
    /// player-interacts-on, entity-interact, and item-stack-interacts-living-entity
    /// calls, run locally
    /// against the real entity. This shell models **none** of it: it does not
    /// carry the entity state (a boat's out-of-control ticks, an animal's
    /// breeding item, a horse's saddle) any of those branches read, and
    /// [`Self::update_entity_target`] keeps only the winning entity's id.
    /// There is therefore no local result to gate on, and the two honest
    /// options are "always" and "never".
    ///
    /// It is "always". **The case this gets wrong is right-clicking a mob
    /// that has no interaction — a zombie, a creeper — where vanilla stays
    /// still and this swings.** The case it gets right is every deliberate
    /// entity right-click (boarding a boat or minecart, mounting a saddled
    /// horse, feeding, shearing, trading), all of which are success with a
    /// client swing source. Unlike the block and generic paths — which have
    /// real local predictions and are now gated on them — closing this one
    /// needs a client-side entity-interact call, not a better guess here.
    fn interact_entity(&mut self, entity_id: i32) {
        let sneaking = self.movement_intent().sneak;
        if let Some(net) = &self.net {
            net.send_action(ClientAction::InteractEntity {
                entity_id,
                interaction: EntityInteraction::Interact { hand: Hand::Main },
                sneaking,
            });
            net.send_action(ClientAction::SwingArm { hand: Hand::Main });
        }
        // Client-side animation, so it runs with or without a socket — the same
        // split `use_item_live` makes for its own `swing_hand`.
        self.swing_hand();
    }

    /// End an attack (attack button released). Aborts a live dig in progress so
    /// the server stops mining; a no-op on the demo world.
    pub fn end_attack(&mut self) {
        if !self.is_live() {
            return;
        }
        let actions = self.write(|w| {
            w.resource_mut::<Attacking>().0 = false;
            w.resource_mut::<MiningPredictor>().0.stop()
        });
        // Sent directly rather than queued: `ActionQueue` is only drained inside
        // the tick loop, so a release on a frame that runs no tick would sit for
        // up to 50 ms before the `ABORT` reached the server. See
        // `crate::interact`'s "how to change it".
        if let Some(net) = &self.net {
            for action in actions {
                net.send_action(action);
            }
        }
    }

    /// Use the held item on the targeted block (use button pressed). On a live
    /// server this lowers the click into the server's `use_item_on` action
    /// through the placement predictor; on the demo world it places directly.
    pub fn use_item(&mut self) {
        if self.is_live() {
            self.use_item_live();
        } else {
            self.place_block();
        }
    }

    /// Release the use button — vanilla's own client entry point: while the
    /// player is using an item, releasing the use key tells the client-side
    /// game-mode handler to release the used item.
    ///
    /// That release handling itself lowers to vanilla's own client-side
    /// release-using-item path sending a bare player-action packet
    /// (a release-use-item action) — [`ClientAction::ReleaseUseItem`] here, encoded
    /// by all four protocol adapters already
    /// (`crates/protocol/{v47,v340,v735,v770}/src/adapter.rs`) but with no
    /// producer anywhere in this shell before this method. Bow, crossbow and
    /// shield are all `useOnRelease() == true`
    /// and structurally cannot
    /// complete a use without this packet — food and potions are
    /// `useOnRelease() == false` and auto-complete on the server's own tick
    /// count, which is exactly why this gap went unnoticed: eating and
    /// drinking still worked.
    ///
    /// A no-op on the demo world (nothing there tracks an in-progress use).
    pub fn end_use(&mut self) {
        if self.is_live() {
            self.end_use_live();
        }
    }

    /// End a completed consumable's local use state and, while the physical use
    /// button is still held, begin its next use. This is the fixed-tick half of
    /// vanilla's held-key polling: the OS only reports the original press edge,
    /// but vanilla's own client-side key handling starts another use once the previous one
    /// has completed.
    ///
    /// Only consumables have a client-known completion duration. Bows, shields
    /// and other release-driven uses remain active until [`Self::end_use_live`]
    /// receives the actual release edge, so this cannot turn a held bow into a
    /// stream of generic use packets.
    pub(crate) fn restart_completed_consumable_if_held(&mut self) {
        let Some(held) = self
            .player_menu()
            .player_native(self.selected_slot())
            .filter(|stack| !stack.is_empty())
            .map(|stack| stack.item().to_string())
        else {
            return;
        };
        let Some(consumable) = lodestone_game::consumable::consumable_for_item(&held) else {
            return;
        };
        let local = self.local;
        let completed = self.write(|world| {
            let using = world.resource::<UsingItem>().0;
            let complete = world
                .resource::<ItemUseTicks>()
                .0
                .is_some_and(|ticks| ticks >= consumable.consume_ticks);
            if !(using && complete) {
                return false;
            }
            world.resource_mut::<UsingItem>().0 = false;
            world.resource_mut::<ItemUseTicks>().0 = None;
            if let Some(mut effects) = world.get_mut::<ItemUseEffects>(local) {
                effects.0 = None;
            }
            true
        });
        if completed {
            // Re-run the original press path, including its food/fullness gate.
            // A bite that makes the player full therefore clears slowdown here
            // and an attempted restart stays inert once authoritative vitals
            // have arrived.
            self.use_item_live();
        }
    }

    /// The live half of [`Self::end_use`], split out the same way
    /// [`Self::begin_attack_live`] is — reachable directly from a test with no
    /// `vanilla_atlas`, since the swing/send logic itself needs no GPU asset.
    ///
    /// A no-op if [`UsingItem`] is already `false`: no button was ever pressed
    /// down (via [`Self::use_item_live`]) for this to be the release edge of.
    /// Sending `RELEASE_USE_ITEM` in that case would still be harmless —
    /// vanilla's own release-using-item handling no-ops whenever
    /// the server has no `useItem` in progress — but there is nothing to
    /// justify sending it for.
    pub(crate) fn end_use_live(&mut self) {
        let local = self.local;
        let (was_using, held_ticks) = self.write(|w| {
            let was_using = {
                let mut using = w.resource_mut::<UsingItem>();
                std::mem::replace(&mut using.0, false)
            };
            // Taken (not read) whether or not the release is
            // actionable, so a use that ends for any reason cannot leave a
            // duration behind for an unrelated later one to inherit.
            let held = w.resource_mut::<ItemUseTicks>().0.take();
            // Cleared the same unconditional way: a stray `Some` here would
            // otherwise survive into the next tick's `compute_movement_intent`
            // read and keep applying a use-item slowdown to a player who is no
            // longer using anything.
            if let Some(mut effects) = w.get_mut::<ItemUseEffects>(local) {
                effects.0 = None;
            }
            (was_using, held)
        });
        if !was_using {
            return;
        }
        // That fix. Before the send, because vanilla's own order is the same:
        // its own client-side release-using-item path runs the client's
        // own release-using-item entity handling — which is what calls
        // the trident's own release-using hook and applies the launch locally — and the
        // `RELEASE_USE_ITEM` packet is the server being told afterwards. The
        // riptide launch is client-predicted in vanilla, which is why it feels
        // instant, and reproducing that ordering is what keeps our reported
        // position and the server's replay in step.
        self.maybe_riptide(held_ticks.unwrap_or(0));
        if let Some(net) = &self.net {
            net.send_action(ClientAction::ReleaseUseItem);
        }
    }

    /// Native inventory index of the chest armour slot — `36..=39` is
    /// feet/legs/chest/head in this crate's native ordering (`lodestone_game::
    /// menu`'s module table), so the chestplate is **38**, not vanilla's own
    /// `Inventory` index.
    const CHEST_ARMOUR_NATIVE_INDEX: usize = 38;

    /// Whether some equipment slot holds a glider, for
    /// [`lodestone_physics::can_glide`].
    ///
    /// Vanilla walks every equipment slot looking for a
    /// glider data component, in its own can-glide-using entity check.
    /// Two deliberate narrowings:
    ///
    /// * **the chest slot only.** Vanilla's loop is over equipment slots, and in
    ///   practice `minecraft:elytra`'s `minecraft:equippable` names `chest`, so a
    ///   held elytra does not glide. Checking every slot would need the whole
    ///   equippable table; checking the backpack would let an elytra in slot 20
    ///   fly.
    /// * **the item id, not the component.** This client's [`ItemStack`]
    ///   components carry damage, enchantments and a custom name, not
    ///   `minecraft:glider`, so the id is the available proxy. A data pack that
    ///   puts `minecraft:glider` on something else will not glide here — a
    ///   feature gap, not a wrong answer, and the fix is a component decode
    ///   rather than anything in this function.
    #[must_use]
    pub(crate) fn glider_equipped(&self) -> bool {
        self.player_menu()
            .player_native(Self::CHEST_ARMOUR_NATIVE_INDEX)
            .filter(|stack| !stack.is_empty())
            .is_some_and(|stack| stack.item().to_string() == "minecraft:elytra")
    }

    /// Start a firework-rocket elytra boost if the held item is a rocket and we
    /// are gliding.
    ///
    /// # Duration, and the one part of it that cannot be predicted
    ///
    /// Vanilla's rocket lives `10 * flightCount + random.nextInt(6) +
    /// random.nextInt(7)` ticks, `flightCount = 1 + fireworks.flightDuration()`
    ///, and boosts on every one of them while
    /// the holder is fall-flying. The two `nextInt` terms are rolled on the
    /// **server's** RNG, and the vanilla *client* never computes them at all: its
    /// copy of the rocket comes from `ClientboundAddEntityPacket` with
    /// `lifetime = 0`, and `if (life > lifetime && level instanceof ServerLevel)`
    /// means the client's rocket simply keeps boosting until the server removes
    /// the entity.
    ///
    /// This client tracks no rocket entity (it does not decode
    /// `DATA_ATTACHED_TO_TARGET`), so it predicts the deterministic floor —
    /// `10 * flightCount` — and no more. The consequence is a boost about five
    /// ticks shorter than vanilla's average, never longer; since the player is
    /// authoritative over their own position there is nothing to desync, only a
    /// slightly weaker boost. Decoding the rocket's attachment is the real fix
    /// and it is a protocol change, not one here.
    ///
    /// `flightDuration` itself is a `minecraft:fireworks` component this client
    /// does not decode either, so `flightCount` is the standard 1-gunpowder
    /// rocket's `2` — 20 ticks.
    fn start_firework_boost_if_gliding(&mut self) {
        if !self.player().fall_flying {
            return;
        }
        let held = self
            .player_menu()
            .player_native(self.selected_slot())
            .filter(|stack| !stack.is_empty())
            .map(|stack| stack.item().to_string());
        if held.as_deref() != Some("minecraft:firework_rocket") {
            return;
        }
        self.write(|w| w.resource_mut::<FireworkBoost>().0 = FIREWORK_BOOST_TICKS);
    }

    /// `TridentItem.releaseUsing`'s riptide branch,
    /// That fix — the driver `lodestone_physics::apply_riptide` was written
    /// for and never had.
    ///
    /// All three gates vanilla checks before the impulse, evaluated here because
    /// none of them is physics state:
    ///
    /// | vanilla | here |
    /// |---|---|
    /// | `timeHeld >= 10` | `held_ticks`, counted by `tick_item_use` |
    /// | `getTridentSpinAttackStrength(stack, player) > 0` | [`Self::riptide_level`] × [`lodestone_physics::riptide_spin_attack_strength`] |
    /// | `isInWaterOrRain() && !isPassenger()` | [`Self::is_in_water_or_rain`], and the passenger component |
    ///
    /// A dry-land release with a Riptide trident therefore does nothing at all
    /// here, exactly as in vanilla — the wet gate is a real gate, not a
    /// decoration on the impulse.
    fn maybe_riptide(&mut self, held_ticks: u32) {
        if held_ticks < RIPTIDE_MIN_HELD_TICKS {
            return;
        }
        let level = self.riptide_level();
        let strength = lodestone_physics::riptide_spin_attack_strength(level);
        if strength <= 0.0 {
            return;
        }
        if !self.is_in_water_or_rain() {
            return;
        }
        let profile = self.profile();
        let collision = self.tick_collision();
        let lodestone_ecs::player::PlayerCollision::View(source) = &collision else {
            return;
        };
        let source = std::sync::Arc::clone(source);
        let mut player = self.player();
        source.with_view(&mut |view| {
            lodestone_physics::apply_riptide(&mut player, view, &profile, strength);
        });
        self.player_mut(|p| {
            p.velocity = player.velocity;
            p.position = player.position;
            p.auto_spin_attack_ticks = player.auto_spin_attack_ticks;
        });
    }

    /// The Riptide level on the held stack, or `0`.
    ///
    /// # The id now comes from the server's own registry, not from arithmetic
    ///
    /// [`lodestone_model::ItemEnchantment`] carries the **session-scoped
    /// `minecraft:enchantment` registry id**, not a name. That id → name table
    /// was always decoded by the v770 adapter's
    /// `ClientRegistries::entry_names` and used to stop there, never leaving the
    /// version crate — so this method resolved `riptide` through
    /// [`RIPTIDE_ENCHANTMENT_ID`], a *derived* index (dynamic registries arrive
    /// sorted by resource location, and `riptide` is the 33rd of 26.2's 43
    /// built-in enchantments). It is now emitted as
    /// `ClientEvent::EnchantmentRegistryNames` and folded into
    /// [`lodestone_ecs::SessionRegistryOrder`], so the real holder id is
    /// available and a data pack that reorders the registry no longer shifts us
    /// onto some *other* enchantment's level.
    ///
    /// **The fallback is still here and is still load-bearing**, because the
    /// table is empty until the server sends it: a pre-`Login` call, or a server
    /// that sends no enchantment registry at all, gets the derived index rather
    /// than "no riptide". `RegistryOrder::enchantment_id`'s own doc explains why
    /// `None` alone cannot distinguish "no such enchantment" from "no table
    /// yet"; the emptiness check below is that distinction, and without it a
    /// server whose registry genuinely lacks `riptide` would silently inherit the
    /// hardcoded id.
    #[must_use]
    fn riptide_enchantment_id(&self) -> i32 {
        self.read(|w| {
            w.get::<lodestone_ecs::SessionRegistryOrder>(self.local)
                .and_then(|order| {
                    if order.0.enchantments().is_empty() {
                        // No table yet — the derived index is the best we have.
                        None
                    } else {
                        // A real table: trust it even when it has no `riptide`,
                        // and let the id-comparison below find nothing. Falling
                        // back here would resolve *some* enchantment at id 32.
                        Some(order.0.enchantment_id("minecraft:riptide").unwrap_or(-1))
                    }
                })
        })
        .unwrap_or(RIPTIDE_ENCHANTMENT_ID)
    }

    #[must_use]
    fn riptide_level(&self) -> u32 {
        let menu = self.player_menu();
        let Some(stack) = menu
            .player_native(self.selected_slot())
            .filter(|stack| !stack.is_empty())
        else {
            return 0;
        };
        // `minecraft:enchantable/trident` is the supported-items tag; the only
        // item in it is the trident itself.
        if stack.item().to_string() != "minecraft:trident" {
            return 0;
        }
        let riptide_id = self.riptide_enchantment_id();
        stack
            .enchantments()
            .iter()
            .find(|enchantment| enchantment.id == riptide_id)
            .map_or(0, |enchantment| enchantment.level)
    }

    /// Vanilla's own is-in-water-or-rain check, that fix.
    ///
    /// The water half is exact — the same [`lodestone_physics::FluidState`] the
    /// tick computed. The rain half is vanilla's own is-raining-at check, which is
    /// raining, and able to see sky at the position, and the precipitation
    /// there is rain; this
    /// client has **no can-see-sky check** (`crate::app::weather`'s own doc records the
    /// same gap for the rain-muffling sound path) and no per-position
    /// precipitation here, so a non-zero rain level stands in for the whole
    /// predicate.
    ///
    /// It fails toward *allowing* a riptide under a roof, where vanilla's server
    /// would refuse the launch. The cost is one corrective teleport in that case;
    /// the alternative — refusing in the open, where riptide is actually used —
    /// would make the feature unreachable.
    #[must_use]
    fn is_in_water_or_rain(&self) -> bool {
        let submerged = self.read(|w| {
            w.get::<lodestone_ecs::player::Submersion>(self.local)
                .is_some_and(|fluid| fluid.0.in_water())
        });
        if submerged {
            return true;
        }
        self.net
            .as_ref()
            .is_some_and(|net| net.shared_weather().snapshot().rain_level > 0.0)
    }

    /// Lower a live right-click into the server's `use_item_on` action **and
    /// predict the placement locally**.
    ///
    /// The server stays authoritative: [`Placement::use_on`] returns the action to
    /// send in *every* branch, so the shell sends it unconditionally (with a
    /// proper prediction sequence) and lets the server decide, exactly as vanilla
    /// does. Because the server owns the sneak state derived from the wire, the
    /// crouch input must have been sent (see
    /// [`send_player_input`](Self::send_player_input)) for a sneak-placement
    /// against a chest/door to suppress the interaction.
    ///
    /// # Why the local write exists
    ///
    /// This method used to send and wait, so a placed block did not exist
    /// client-side until the server's `BLOCK_UPDATE` came back — one round trip of
    /// hole. For a chest that is that fix reached through a different door: the state
    /// write is what creates the block entity, and with no local state write there
    /// was no local record and nothing to draw. The prediction now writes through
    /// [`write_predicted_block`], the same `set_block` + `sync_block_entity` pair
    /// the adapter's `BLOCK_UPDATE` arm calls.
    ///
    /// # What happens when the server refuses
    ///
    /// Nothing here has to detect it, because vanilla's server corrects **both**
    /// candidate positions after *every* `use_item_on`, unconditionally — accepted,
    /// refused, or actually an interaction
    ///:
    ///
    /// ```text
    /// this.send(new ClientboundBlockUpdatePacket(level, pos));
    /// this.send(new ClientboundBlockUpdatePacket(level, pos.relative(direction)));
    /// ```
    ///
    /// `pos` is `clicked` and `pos.relative(direction)` is the adjacent cell, and a
    /// prediction can only ever land on one of those two. So a refused placement is
    /// overwritten by the authoritative state within one round trip — and since
    /// That fix that path calls `sync_block_entity`, which **removes** the block-entity
    /// record the prediction created (`BlockEntitySync::Removed`). The removal half
    /// is not a second mechanism to build; it is the same one, pointing the other
    /// way. `crates/lodestone-shell/tests/placed_chest_block_entity_pixels.rs`
    /// gates it rather than assuming it.
    ///
    /// A mispredicted placement therefore costs exactly the round trip the hole
    /// used to cost, which is why every classification below is allowed to err
    /// toward *not* predicting but never toward predicting something wrong.
    ///
    /// # Cancellation boundary
    ///
    /// One [`VerbContext::PlayerInteract`] ask covers the entity, block, and air
    /// branches. It runs before held-use or firework state, placement/equipment
    /// prediction, sequence allocation, swings, sounds, and wire sends. A denial
    /// is therefore a complete no-op. The entity-first target choice is captured
    /// for the context and reused by the allowed branch so the ask and commit
    /// cannot describe different targets.
    pub(crate) fn use_item_live(&mut self) {
        if self.is_dead() {
            return;
        }
        // Snapshot the same entity-first target choice the commitment branches
        // below use, then ask once before any local state or prediction changes.
        // Reusing these snapshots also guarantees the context names the target
        // this click actually commits if another system updates the ray later.
        let entity_target = self.entity_target();
        let block_target = if entity_target.is_some() {
            None
        } else {
            self.target()
        };
        let context = VerbContext::PlayerInteract {
            pos: block_target.map(|hit| {
                BlockPos::new(hit.block[0], hit.block[1], hit.block[2])
            }),
            target_entity_id: entity_target,
        };
        let vetoed = self.read(|world| {
            world
                .get_resource::<ActionVetoes>()
                .is_some_and(|vetoes| vetoes.allows(&context) == Verdict::Deny)
        });
        if vetoed {
            return;
        }
        // **Gated on the held item actually having a use, matching
        // vanilla's own use-animation lookup — see [`item_has_use_animation`].** Before
        // this gate, [`UsingItem`]/[`ItemUseEffects`] were armed for *every*
        // right click, so aiming at open air with an empty hand (or any item
        // with no use at all, a sword included) applied the same
        // `UseEffects::DEFAULT` movement slowdown as eating: `isUsingItem()`
        // in vanilla only becomes true when the item's own `use()` starts the
        // item-use state, and the base item's `use()` — which is what a
        // sword, a block or an empty hand all fall back to — never does.
        let held = self
            .player_menu()
            .player_native(self.selected_slot())
            .filter(|stack| !stack.is_empty())
            .map(|stack| stack.item().to_string());
        let local = self.local;
        let (food_level, invulnerable) = self.read(|w| {
            let food_level = w.get::<Vitals>(local).and_then(|v| v.food);
            let invulnerable = w
                .get::<ServerGameMode>(local)
                .and_then(|mode| mode.0)
                .is_some_and(|mode| !crate::hud::can_hurt_player(Some(mode)));
            (food_level, invulnerable)
        });
        let uses_item = held
            .as_deref()
            .is_some_and(|id| item_can_start_use(id, food_level, invulnerable));
        if uses_item {
            // Marks [`UsingItem`] so a later [`Self::end_use`] knows the
            // button was actually pressed — see that resource's own docs for
            // why this is an input-state mirror rather than vanilla's real
            // `isUsingItem()`.
            self.write(|w| w.resource_mut::<UsingItem>().0 = true);
            // Arm vanilla's `timeHeld`, which is what
            // `TridentItem.releaseUsing` compares against its 10-tick threshold on
            // the release edge. Zero here and advanced by
            // `lodestone_ecs::player::tick_item_use`, so the count is in 20 Hz ticks
            // and not in frames — a 200 fps client must not reach the threshold ten
            // times sooner than a 20 fps one.
            self.write(|w| w.resource_mut::<ItemUseTicks>().0 = Some(0));
            // [`ItemUseEffects`]'s writer: resolve which
            // [`UseEffects`] the held main-hand item arms. `UseEffects::for_item`
            // itself still only distinguishes spear-vs-default — `uses_item`
            // above is what keeps a non-use item from ever reaching it in the
            // first place, since `for_item` has no "no effect at all" case of
            // its own (see that function's own doc). Set once, for the
            // duration of the press — mirroring how `UsingItem`/`ItemUseTicks`
            // above are also start/end edges rather than re-derived every
            // tick, since vanilla itself cannot change which item a use is
            // charging mid-use.
            let use_effects = held
                .as_deref()
                .map_or(UseEffects::DEFAULT, UseEffects::for_item);
            self.write(|w| {
                if let Some(mut effects) = w.get_mut::<ItemUseEffects>(local) {
                    effects.0 = Some(use_effects);
                }
            });
        }
        // A firework rocket used while gliding is the boost, and it
        // is not an interaction with anything the branches below resolve — so it
        // is decided here, before them, off the held stack alone.
        self.start_firework_boost_if_gliding();
        // **Entity before block, and this branch is the whole of "get in a boat".**
        // Vanilla's own per-hand use-item dispatch switches on the hit-result type and
        // its entity case comes first (vanilla's own client entry point's use-item
        // dispatch), the identical
        // priority [`Self::begin_attack_live`] already implements for the left
        // button off the same [`EntityRayTarget`]. Before this, `use_item_live`
        // returned early on `self.target()` being `None` and never looked at the
        // entity ray at all, so a right-click on a boat, minecart or saddled horse
        // sent nothing — the mount packet had no producer, which is the outbound
        // half of the island `EntityPassengersChanged` was the inbound half of.
        //
        // `InteractAt` is deliberately **not** used, even though vanilla sends both
        // it and `Interact` for a `case ENTITY` click: `InteractAt` carries the
        // entity-local hit position, and [`Self::update_entity_target`] keeps only
        // the winning entity's id, not the ray's hit point on its box. A fabricated
        // local offset would be a wrong number where the server accepts a missing
        // one — `ServerGamePacketListenerImpl` dispatches mounting off the plain
        // `Interact` (it is `Entity.interact` that returns `InteractionResult` and
        // calls `player.startRiding`), and `InteractAt` only matters for the
        // per-part hit an armour stand or a horse's saddle slot resolves. So the
        // honest subset is sent, and refining it needs the ray to start reporting
        // its hit position, not a guess here.
        //
        // **`case ENTITY` only returns here on a *successful* interact.**
        // Vanilla's own switch returns
        // immediately only when `gameMode.interact(...) instanceof
        // InteractionResult.Success`; anything else hits an explicit `break;`
        // at `:1708` and falls through to the unconditional generic-use call
        // at `:1730` (`gameMode.useItem`) — which is what actually raises a
        // shield or starts drawing a bow when the crosshair happens to be
        // over a mob with no special right-click behaviour (hostile mobs,
        // overwhelmingly, which is exactly the combat case). Before this fix
        // `use_item_live` always returned here, so `entity_target()` being
        // `Some` for *any* living entity in `ENTITY_REACH` — hostile or not —
        // permanently short-circuited the fallback.
        //
        // This client has no local classification of an interact's result to
        // match vanilla's `instanceof Success` test against: there is no
        // `player.interactOn` equivalent here, only the wire send (the same
        // gap `Self::interact_entity`'s own docs cover for why `InteractAt`
        // is not fabricated). So every entity interact is treated as
        // non-consuming for this decision and always falls through to
        // [`Self::use_item_generic`]. The one place this can diverge from
        // vanilla is a genuinely successful mount (an empty boat, a saddled
        // and rideable horse): vanilla's own local prediction would skip the
        // fallback there, and this does not, so an item held while boarding a
        // vehicle can also start its use. That is judged the smaller error
        // next to a shield/bow that could never fire at all.
        if let Some(entity_id) = entity_target {
            self.interact_entity(entity_id);
            // `already_swung`: [`Self::interact_entity`] has just swung (see its
            // doc for why that is unconditional). Vanilla reaches at most one
            // `player.swing` per `startUseItem` — its `case ENTITY` *returns*
            // when it swings — so letting the fall-through swing a second time
            // would put two `SwingArm` packets on the wire for one click.
            self.use_item_generic(true);
            return;
        }
        let Some(hit) = block_target else {
            // Vanilla's own MISS/no-target path: a `null` `hitResult` skips
            // the whole `if (this.hitResult != null)` switch in
            // `startUseItem` and still reaches
            // the unconditional fallback at `:1730`. This used to `return`
            // here with **nothing sent at all** — aiming at open air, or at a
            // mob standing just past block reach with nothing behind it,
            // silently dropped the click.
            self.use_item_generic(false);
            return;
        };
        let clicked = BlockPos::new(hit.block[0], hit.block[1], hit.block[2]);
        let face = face_from_normal(hit.normal);
        let cursor = hit_cursor(hit);
        // The intent this tick's physics ran on — the same one
        // `lodestone_controller::ecs::send_player_input` derived the wire's shift
        // bit from, so the local decision and the server's cannot disagree. This
        // used to re-read the keyboard, which was frame-granular; vanilla is
        // tick-granular here too (`Minecraft.handleKeybinds` runs in the tick).
        let sneaking = self.movement_intent().sneak;

        let menu = self.player_menu();
        let main = menu
            .player_native(self.selected_slot())
            .filter(|stack| !stack.is_empty())
            .map(|stack| stack.item().clone());
        // Vanilla's `haveSomethingInOurHands` — *either* hand, and it is what
        // makes a sneak-click suppress the block's own use.
        let has_item_in_hand = main.is_some()
            || menu
                .player_native(crate::sim::OFFHAND_NATIVE_INDEX)
                .is_some_and(|stack| !stack.is_empty());
        // Placeable only when the census can name the block *and* classify how it
        // orients. Leaving `placing` at `None` otherwise is what makes an
        // unclassifiable item fall back to send-and-wait rather than write a state
        // we are not confident in.
        let placeable = main.as_ref().and_then(|item| {
            let name = item.to_string();
            let states = block_states_of(&name)?;
            let orientation = orientation_for_placement(&name, &states)?;
            Some((name, states, orientation))
        });
        let ctx = UseOnContext {
            hand: Hand::Main,
            clicked,
            face,
            cursor,
            inside_block: false,
            rotation: Rotation::new(self.player().yaw, self.player().pitch),
            sneaking,
            has_item_in_hand,
            placing: placeable.as_ref().and_then(|_| main.clone()),
            orientation: placeable
                .as_ref()
                .map_or(OrientationKind::Fixed, |&(_, _, kind)| kind),
        };
        // Read the world facts before taking the ECS guard `use_on` needs — see
        // `PlacementFacts` on why the two guards must not nest. Free function
        // since `PlaceIntent` (`crate::interact::drive_placement`, a `GameTick`
        // system, needs the identical resolution with no `Sim` to call it on)
        // — see `sim/placement.rs`'s `placement_facts` doc.
        let bb = self.player().bounding_box(&self.profile());
        let facts = placement_facts(
            clicked,
            face,
            |pos| self.net.as_ref().and_then(|net| net.block_at(pos)),
            |pos| block_intersects_player(&bb, [pos.x, pos.y, pos.z]),
        );
        let decision = self.write(|w| {
            w.resource_mut::<PlacementPredictor>()
                .0
                .use_on(&ctx, &facts)
        });
        let (UseOnDecision::Interact { action }
        | UseOnDecision::Place { action, .. }
        | UseOnDecision::Nothing { action }) = &decision;
        // **The swing is the block result's, not the click's.**
        // `Minecraft.startUseItem`'s `case BLOCK` calls `player.swing(hand)`
        // only when `gameMode.useItemOn(...)` returned an
        // `InteractionResult.Success` whose `swingSource()` is `CLIENT`, and
        // `MultiPlayerGameMode.performUseItemOn` computes that result
        // *locally* — which is exactly what [`Placement::use_on`] is, so
        // unlike the entity path this decision is one we hold:
        //
        // * `Interact` — the block actuated. A door, a lever, a chest, a
        //   crafting table, a note block all return `InteractionResult.SUCCESS`
        //   from `useWithoutItem`/`useItemOn`. Swing.
        // * `Place` — `BlockItem.place` returns `SUCCESS`. Swing.
        // * `Nothing` — `use_on` could not name an interaction, which covers
        //   both the base `Item.useOn`'s `PASS` and the overrides that return
        //   `SUCCESS`. [`use_on_block_swings`] separates them by item id; see
        //   its doc for the approximation and which case it gets wrong.
        //
        // Before this the swing was unconditional here, so right-clicking
        // plain stone with a sword — vanilla's `PASS`, and silent — swung the
        // arm and put a `SwingArm` on the wire for every other player to see.
        let swings = match &decision {
            UseOnDecision::Interact { .. } | UseOnDecision::Place { .. } => true,
            UseOnDecision::Nothing { .. } => main
                .as_ref()
                .is_some_and(|item| use_on_block_swings(&item.to_string())),
        };
        if let Some(net) = &self.net {
            net.send_action(action.clone());
            if swings {
                net.send_action(ClientAction::SwingArm { hand: Hand::Main });
            }
        }
        // This swing bypasses `ActionQueue` (the send above goes straight to
        // the socket so its wire order is fixed), so it also bypasses
        // `drain_action_queue`'s hook and has to start the animation itself.
        // Outside the `if let` above: the animation is client-side and does
        // not need a socket.
        if swings {
            self.swing_hand();
        }

        // The prediction. `placeable` is `Some` whenever `use_on` could have
        // returned `Place` at all (it is what filled `ctx.placing`), so the only
        // way this declines is `state_for_placement` failing on a property it
        // cannot resolve.
        if let (UseOnDecision::Place { prediction, .. }, Some((name, states, orientation))) =
            (&decision, &placeable)
        {
            if let Some(state) = state_for_placement(name, states, *orientation, &prediction.state) {
                let pos = prediction.pos;
                self.predict_block([pos.x, pos.y, pos.z], state);
                // Vanilla's placement sound is the tail of its own block-item
                // place path
                //, which passes the placing player as
                // its own sound-play's **excluded** entity — so the server broadcasts it
                // to everyone but us, and our own copy is predicted locally by
                // vanilla's own client-side sound-play, whose exclusion test is inverted
                // (it plays only for the excluded entity, on the client that is
                // the local player).
                // It therefore hangs off the prediction, exactly as vanilla's
                // does: no prediction, no sound, and no double-play either.
                //
                // Tied to the *predicted state* rather than to the item, because
                // the sound is the placed state's own sound type — a waterlogged or
                // half-slab placement can be a different sound type from the
                // block's default state.
                self.play_block_place_sound([pos.x, pos.y, pos.z], state);
            }
        }

        // **`case BLOCK`'s fall-through — the only route by which any `use`-based
        // item works while the crosshair is on a block.** The server's
        // `ServerPlayerGameMode.useItemOn` never reaches `Item.use`, so a boat,
        // food, a drink, an equip-on-use helmet and a bow draw are
        // all reachable *only* through `USE_ITEM`. Until this branch existed the
        // block path `return`ed after its two sends, so every one of those worked
        // when aimed at open air or at a mob and did nothing when aimed at a
        // block — which for a boat means it places over deep water (where the
        // block ray misses) and never on a shoreline.
        //
        // **It is not unconditional, and vanilla's `case BLOCK` is not a `break`.**
        // Unlike `case ENTITY` (which has an explicit `break` and always falls
        // through), `Minecraft.startUseItem`'s `case BLOCK` `return`s on
        // `InteractionResult.Success` *and* on `InteractionResult.Fail`, and reaches
        // the generic use only for a non-consuming result. So the condition here is
        // "what would `MultiPlayerGameMode.performUseItemOn` have returned":
        //
        // * [`UseOnDecision::Interact`] / [`UseOnDecision::Place`] — vanilla's
        //   `Success` (the block actuated, or `BlockItem.useOn` placed). Return.
        // * [`UseOnDecision::Nothing`] with a placeable item — vanilla's `Fail`
        //   (`BlockItem.place` refused: obstructed, or a non-replaceable target).
        //   Return. Falling through here is what would make a **carved pumpkin**
        //   aimed at an illegal face equip itself onto the player's head instead of
        //   doing nothing — it is both a placeable block and an `equippable`.
        // * [`UseOnDecision::Nothing`] with no placeable item — vanilla's default
        //   `Item.useOn`, which is `PASS`. This is the boat, the food, the potion.
        //
        // `placeable.is_none()` is therefore standing in for "the held item has no
        // `useOn` of its own". It is the same shape of over-approximation
        // `is_interactable_state` makes for blocks, and it
        // errs the same safe way: an item our block census cannot name is treated as
        // non-placeable, so at worst a `USE_ITEM` follows a placement the server
        // accepted, where `Item.use` is `PASS` for a plain `BlockItem` anyway.
        //
        // **`!swings` is the third clause, and it is the same `return` vanilla
        // takes.** `Nothing` with a non-placeable item is only vanilla's `PASS`
        // when the item has no `useOn` of its own; when [`use_on_block_swings`]
        // says it does, the result was `SUCCESS` and `case BLOCK` returns
        // without ever reaching `gameMode.useItem`. Flint and steel lights the
        // block and stops there — it does not also generic-use itself.
        if matches!(decision, UseOnDecision::Nothing { .. }) && placeable.is_none() && !swings {
            self.use_item_generic(false);
        }
    }

    /// Vanilla's unconditional generic-use fallback at the bottom of
    /// its own per-hand use-item dispatch loop (its own client-side
    /// game-mode's use-item call) — the send that actually raises a shield, draws a
    /// bow, or starts eating/drinking, independent of any block or entity
    /// under the crosshair. Called from [`Self::use_item_live`]'s entity and
    /// no-target branches; see that method's docs for exactly which vanilla
    /// cases reach it.
    ///
    /// Lowers to [`ClientAction::UseItem`] — a **second** serverbound island
    /// this investigation found alongside `ReleaseUseItem`: encoded by all
    /// four protocol adapters
    /// (`crates/protocol/{v47,v340,v735,v770}/src/adapter.rs`) with zero
    /// producers anywhere in this shell before this method.
    ///
    /// Guarded on the main hand actually holding something, matching
    /// vanilla's own `!heldItem.isEmpty()` check at the same call site —
    /// there is nothing to use and no packet to justify for an empty hand.
    /// Only `Hand::Main` is considered, matching every other send in this
    /// method; vanilla's per-hand loop also tries the off hand, which this
    /// shell does not model here.
    ///
    /// The prediction sequence is borrowed from [`PlacementPredictor`]'s own
    /// counter rather than a second, independent one — see
    /// [`Placement::take_use_sequence`]'s docs for why that matches vanilla's
    /// own single shared counter.
    ///
    /// # Armour equip prediction
    ///
    /// This is also the one place [`Self::predict_equip_swap`] can fire: an
    /// armour piece right-clicked from the hotbar has no block or entity
    /// target and no special-cased use of its own, so vanilla's
    /// per-hand use-item dispatch falls all the way through to its own
    /// game-mode use-item call — this method's send — exactly like a shield raise or a bow
    /// draw. See that method's own doc for why the equip write belongs here
    /// rather than in [`Self::use_item_live`].
    /// # The swing, and why it is no longer unconditional
    ///
    /// Vanilla's own per-hand use-item dispatch's fallback swings only when
    /// its own game-mode use-item call returns an unqualified success whose
    /// swing source is the client. Its own client-side use-item path computes
    /// that by running the item's own use call locally, so — as on the block
    /// path — the decision is one the client owns rather than one it waits a
    /// round trip for. [`generic_use_swings`] is that decision ported by item
    /// id, plus [`Self::predict_equip_swap`] for the one branch of the base
    /// item's use that depends on the live menu rather than on the id.
    ///
    /// This is the owner's report — *"right clicking with (i think) any item
    /// makes me swing my arm"* — and it was: a drawn bow, a raised shield, a
    /// bite of food, a spyglass and an idle sword all return
    /// a "consume" or "pass" result, all of which are silent in
    /// vanilla, and every one of them swung here and put a `SwingArm` on the
    /// wire. A snowball, an ender pearl, a fishing rod and a book still do
    /// swing, because those really are a plain success.
    ///
    /// `already_swung` is the entity path's: [`Self::interact_entity`] has
    /// swung before this is reached and vanilla never swings twice for one
    /// `startUseItem`.
    fn use_item_generic(&mut self, already_swung: bool) {
        let held = self
            .player_menu()
            .player_native(self.selected_slot())
            .cloned();
        let Some(held) = held.filter(|stack| !stack.is_empty()) else {
            return;
        };
        // Reported, not assumed: vanilla's own equip-swap component logic is
        // success only when the swap actually happens ("fail" when the
        // armour slot already holds the same item, "pass" when the slot is not
        // usable), and that is precisely the condition this predicts.
        let equipped = self.predict_equip_swap(&held);
        let swings = !already_swung
            && (equipped || generic_use_swings(&held.item().to_string(), self.player().fall_flying));
        let rotation = Rotation::new(self.player().yaw, self.player().pitch);
        let sequence =
            self.write(|w| w.resource_mut::<PlacementPredictor>().0.take_use_sequence());
        if let Some(net) = &self.net {
            net.send_action(ClientAction::UseItem {
                hand: Hand::Main,
                rotation,
                sequence,
            });
            if swings {
                net.send_action(ClientAction::SwingArm { hand: Hand::Main });
            }
        }
        // Client-side animation, so it runs with or without a socket — the
        // same split every other swing site in this method makes.
        if swings {
            self.swing_hand();
        }
    }

    /// Predicts vanilla's own item `use()` falling into its own equip-swap
    /// component logic — the branch that actually equips a
    /// helmet/chestplate/leggings/boots right-clicked from the hotbar with no
    /// block or entity under the crosshair (or one whose own interaction
    /// declined). Before this, [`Self::use_item_generic`] sent
    /// [`ClientAction::UseItem`] and wrote nothing locally, so armour only
    /// appeared once the server's own `SET_SLOT` pair came back — the round
    /// trip the "missing client prediction" report was about.
    ///
    /// # Reconciliation is the same fold a real `SET_SLOT` takes
    ///
    /// The write goes in as two synthesized [`ClientEvent::ContainerSlot`]s —
    /// the same idiom [`Self::apply_creative_slot`] uses for `SET_CREATIVE_
    /// MODE_SLOT` — against the **same menu-slot indices** the wire uses:
    /// the armour slot's [`EquipmentSlot::player_menu_index`] (`5..=8`) and
    /// the hotbar's `36 + selected_slot`. Vanilla's own server-side use-item
    /// handling
    /// mutates the player's inventory menu (window 0) directly, and that menu's
    /// own change-broadcast diff runs every tick regardless of which action
    /// changed it, so the exact same `ContainerSlot` event — this time
    /// server-authoritative — arrives again for both slots shortly after.
    /// `Menu::reconcile` (`lodestone_game::reconcile`, reached through
    /// `Menus::apply`) is what corrects a wrong guess: it always overwrites
    /// both the confirmed and (where they differ) the predicted contents, so
    /// a matching echo is a silent no-op and a disagreeing one snaps the
    /// slot to the truth. There is nothing bespoke to add for the
    /// disagreement case — it is the same path a chest's shift-click already
    /// exercises.
    ///
    /// # Scope
    ///
    /// * Only the four `HUMANOID_ARMOR` positions predict — see
    ///   [`EquipmentSlot::player_menu_index`]'s own doc for why the off-hand
    ///   slot is excluded (no real item swaps into it this way).
    /// * Only a `count <= 1` held stack predicts, matching vanilla's own
    ///   equip-swap logic's own held-count-at-most-one branch — every
    ///   shipped armour item has a max stack size of 1, so this covers
    ///   ordinary play; a hypothetical equippable item with a larger cap
    ///   falls back to send-and-wait rather than modelling vanilla's partial-
    ///   consume branch (a one-item partial consume-and-return).
    /// * Not modelled: non-swappable items and a target slot carrying
    ///   a `minecraft:prevent_armor_change`-effect enchantment — vanilla's
    ///   own equippable data component and [`lodestone_model::ItemComponents`]
    ///   both carry only the *slot*, per that type's own doc ("Only the slot
    ///   is carried"), so there is no flag here to gate on. A server that
    ///   refuses for either reason is corrected by the same reconcile path
    ///   above, at the cost of one visible snap-back instead of silence.
    ///
    /// # Return value
    ///
    /// Whether the swap was predicted — i.e. whether
    /// vanilla's own equip-swap component logic would have returned
    /// a plain success rather than its "fail"/"pass" arms. This is
    /// [`Self::use_item_generic`]'s swing predicate for equippables: it is the
    /// one success the base item's `use` can produce, and it cannot be
    /// answered from the item id alone because it depends on what the armour
    /// slot already holds. `false` for every early return below, including
    /// the `already the same item` case vanilla `FAIL`s on — vanilla's test
    /// there is `ItemStack.isSameItemSameComponents`, which ignores count;
    /// this compares whole stacks, and the two agree because both sides of a
    /// same-item comparison are a count-1 armour piece.
    fn predict_equip_swap(&mut self, held: &lodestone_game::item::ItemStack) -> bool {
        if held.count() > 1 {
            return false;
        }
        let Some(target) = lodestone_game::container::equippable_slot(held) else {
            return false;
        };
        let Some(armor_menu_slot) = target.player_menu_index() else {
            return false;
        };
        let previous = self.player_menu().slot_item(armor_menu_slot).cloned();
        if previous.as_ref() == Some(held) {
            return false;
        }
        let hotbar_menu_slot = 36 + self.selected_slot();
        let held_model = lodestone_model::ItemStack::from(held);
        let previous_model = previous.as_ref().map(lodestone_model::ItemStack::from);
        self.write_local(|w, local| {
            if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
                // The current state id, unchanged — same reasoning as
                // `apply_creative_slot`: nothing about this write advances the
                // container's synchronisation counter.
                let state_id = menus.0.player().state_id() as i32;
                menus.0.apply(&lodestone_model::ClientEvent::ContainerSlot {
                    window_id: 0,
                    state_id,
                    slot: i32::try_from(armor_menu_slot).unwrap_or(0),
                    item: Some(held_model),
                });
                menus.0.apply(&lodestone_model::ClientEvent::ContainerSlot {
                    window_id: 0,
                    state_id,
                    slot: i32::try_from(hotbar_menu_slot).unwrap_or(0),
                    item: previous_model,
                });
            }
        });
        true
    }

    /// Apply a locally predicted block state to the one chunk store and re-mesh.
    ///
    /// The write itself is [`write_predicted_block`] — state *and* block entity,
    /// the adapter's `BLOCK_UPDATE` pair — so a predicted chest exists as a
    /// block-entity record the moment it is placed instead of one round trip
    /// later.
    fn predict_block(&mut self, block: [i32; 3], state: u32) -> BlockEntitySync {
        let store = self.chunk_world_write();
        // The chunk guard is taken and dropped before `remesh_around` reaches for
        // the ECS resource again, so the two are never held together.
        let outcome = {
            let mut world = store.write();
            write_predicted_block(&mut *world, block, state)
        };
        self.remesh_around(block);
        outcome
    }

    /// Place [`PLACE_BLOCK`] against the targeted face on the **demo world**, if
    /// the cell is empty and doesn't intersect the player. Returns whether a
    /// block was placed. The live path uses [`use_item`](Self::use_item) instead
    /// so the server actually hears the placement.
    pub fn place_block(&mut self) -> bool {
        let Some(hit) = self.target() else {
            return false;
        };
        let pos = hit.place_position();
        let cell_empty = {
            let store = self.chunk_world();
            let world = store.read();
            let view = WorldCollision::new(&world);
            view.block_at(pos[0], pos[1], pos[2]) == id::AIR
        };
        let bb = self.player().bounding_box(&self.profile());
        if !cell_empty || block_intersects_player(&bb, pos) {
            return false;
        }
        if self.set_block_world(pos, PLACE_BLOCK) {
            self.remesh_around(pos);
            // Demo-world placement, same reasoning as `break_block`.
            self.swing_hand();
            true
        } else {
            false
        }
    }
}
