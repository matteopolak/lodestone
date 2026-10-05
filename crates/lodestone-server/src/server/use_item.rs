//! Using the held item: bows and projectiles, eating and drinking, eyes of ender, and the release-use and consume bookkeeping.

use super::*;

/// One in-progress bow draw: which tick it started on, and the facing the
/// `USE_ITEM` reported.
///
/// The facing is captured at the *start* and used as a fallback only. Vanilla
/// shoots along the player's facing at **release**, which `player_rot` supplies if
/// the client has ever sent angles — so this field only matters for a connection
/// that draws and releases without having sent a single rotation packet, where the
/// alternative would be firing due south.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BowDraw {
    /// The `MobSim::tick_count` the draw began on.
    pub(super) started_tick: u64,
    /// The facing the `USE_ITEM` packet carried.
    pub(super) yaw: f32,
    /// The pitch the `USE_ITEM` packet carried.
    pub(super) pitch: f32,
}

/// The item a `USE_ITEM` is asking to launch, and how.
///
/// A closed enum rather than a string match at the call site, because the two
/// behaviours are genuinely different shapes: a throwable resolves entirely inside
/// the `USE_ITEM` arm, and a bow resolves in a *later* packet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum LaunchIntent {
    /// Thrown the instant the packet arrives, at
    /// [`THROWABLE_SHOOT_POWER`](lodestone_entity::projectile::THROWABLE_SHOOT_POWER):
    /// snowball, egg, ender pearl. The projectile entity is the item's own name.
    InstantThrow {
        /// The projectile entity type to spawn.
        projectile: &'static str,
        /// Launch speed in blocks per tick.
        power: f64,
        /// Vanilla's `yOffset`, non-zero only for a potion.
        pitch_offset: f64,
    },
    /// Starts a draw the release packet finishes.
    BeginDraw,
}

/// What the item in `path` does on a right-click in mid-air.
///
/// Only the items that actually launch something are listed. Everything else —
/// food, blocks, a bucket — is `None`, and the `USE_ITEM` arm does nothing, which
/// is correct rather than unimplemented for a crate with no eating or placement
/// model on this packet.
pub(super) fn launch_intent(path: &str) -> Option<LaunchIntent> {
    use lodestone_entity::projectile::{
        POTION_PITCH_OFFSET, POTION_SHOOT_POWER, THROWABLE_SHOOT_POWER,
    };
    let throw = |projectile| {
        Some(LaunchIntent::InstantThrow {
            projectile,
            power: THROWABLE_SHOOT_POWER,
            pitch_offset: 0.0,
        })
    };
    match path {
        "snowball" => throw("snowball"),
        "egg" => throw("egg"),
        "ender_pearl" => throw("ender_pearl"),
        "experience_bottle" => throw("experience_bottle"),
        // `ThrowablePotionItem`: slower, and the only one with a pitch offset.
        "splash_potion" => Some(LaunchIntent::InstantThrow {
            projectile: "splash_potion",
            power: POTION_SHOOT_POWER,
            pitch_offset: POTION_PITCH_OFFSET,
        }),
        "lingering_potion" => Some(LaunchIntent::InstantThrow {
            projectile: "lingering_potion",
            power: POTION_SHOOT_POWER,
            pitch_offset: POTION_PITCH_OFFSET,
        }),
        // A crossbow's charge/hold semantics are genuinely different (it stores a
        // loaded projectile in a component and fires on the *next* use), and there
        // is no charged-projectiles component model here, so it is deliberately not
        // folded in with the bow — a shared arm would fire it like a bow, which is
        // wrong in a way that looks right.
        "bow" => Some(LaunchIntent::BeginDraw),
        _ => None,
    }
}

/// The ammunition a drawn bow consumes, and whether the inventory has any.
///
/// The ammunition search matches the weapon's ammo predicate; this crate models
/// the plain arrow only, which is the ammunition a standard bow finds first.
pub(super) const BOW_AMMUNITION: &str = "arrow";

/// One consume (eat or drink) in progress on a connection.
///
/// The item-use state records the two facts completion needs: which slot is
/// being eaten from, and when it
/// finishes. `item` is carried so a slot whose contents changed mid-bite (a
/// container click, a hotbar swap) cannot complete as if it were still the food
/// The same "re-check what you recorded" guard `PendingBreak` applies to a dig.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ItemInUse {
    /// Native inventory index the food is in.
    pub(super) native: usize,
    /// The item that started the use, full registry name.
    pub(super) item: String,
    /// The `MobSim` tick the use completes on — `started` plus the item's consume-ticks value.
    pub(super) finish_tick: u64,
    /// The `remaining` value the last periodic consume sound was published for.
    ///
    /// The consumable emit-particles-and-sounds predicate is
    /// `remaining % 4 == 0`, which is correct **only if it is evaluated exactly once
    /// per tick**. The loop that drives it reads `MobSim`'s counter from a 50 ms
    /// timer arm, and the two clocks are not the same object: if the timer fires
    /// twice inside one mob tick, the same `remaining` passes the predicate again and
    /// the eating sound doubles. Latching the value it last fired for makes the
    /// emission idempotent per tick without assuming the clocks agree.
    pub(super) last_effect_remaining: Option<u32>,
}

/// What a `USE_ITEM` started. This is the subset of item-use outcomes with a
/// consequence here.
#[derive(Debug)]
pub(super) enum UseItemOutcome {
    /// Nothing this crate models.
    Nothing,
    /// A bow draw opened; the `RELEASE_USE_ITEM` that follows ends it.
    Draw(BowDraw),
    /// A consume opened; the server's own clock ends it.
    Consuming(ItemInUse),
    /// An equip swap already happened; this arm is instantaneous.
    Equipped(crate::item_use::EquipSwap),
}

/// Where an eye of ender is launched, as a fraction of the player's standing
/// height above their feet.
pub(super) const EYE_LAUNCH_HEIGHT_FRACTION: f64 = 0.5;

/// The standing player's collision height, which the launch height is a
/// fraction of.
pub(super) const PLAYER_STANDING_HEIGHT: f64 = 1.8;

/// Throws the held eye of ender at the nearest stronghold, if there is one.
///
/// Returns the launch sound when an eye was thrown and `None` when nothing
/// happened, in which case the stack is untouched. Nothing happens in a
/// dimension other than the overworld, when `locate` finds no stronghold, when
/// the player is looking at an end portal frame (that click belongs to the
/// frame-filling arm), or when the stack is empty.
///
/// `sound_roll` is a uniform `[0, 1)` draw that sets the launch sound's pitch
/// between 0.33 and 0.5.
#[allow(clippy::too_many_arguments)]
pub(super) fn launch_eye_of_ender(
    mobs: &MobHandle,
    inventory: &mut PlayerInventory,
    native: usize,
    game_mode: GameMode,
    feet: Vec3,
    yaw: f32,
    pitch: f32,
    dimension: crate::dimension::Dimension,
    block_state: &dyn Fn(i32, i32, i32) -> StateId,
    locate: &dyn Fn(BlockPos) -> Option<BlockPos>,
    sound_roll: f32,
) -> Option<crate::effects::WorldEffect> {
    if dimension != crate::dimension::Dimension::Overworld
        || inventory
            .native(native)
            .is_none_or(|stack| stack.item.path() != "ender_eye")
    {
        return None;
    }
    let eye = Vec3::new(feet.x, feet.y + EYE_HEIGHT, feet.z);
    let reach = crate::boat::block_interaction_range(game_mode == GameMode::Creative);
    let view = crate::boat::view_direction(yaw, pitch);
    let end = Vec3::new(eye.x + view.x * reach, eye.y + view.y * reach, eye.z + view.z * reach);
    if let Some(hit) = crate::boat::clip(eye, end, block_state)
        && crate::portal::is_end_portal_frame(block_state(hit.cell.x, hit.cell.y, hit.cell.z))
    {
        return None;
    }
    let target = locate(BlockPos::new(
        feet.x.floor() as i32,
        feet.y.floor() as i32,
        feet.z.floor() as i32,
    ))?;
    if !consume_one(inventory, native, game_mode) {
        return None;
    }
    let launch = Vec3::new(
        feet.x,
        feet.y + PLAYER_STANDING_HEIGHT * EYE_LAUNCH_HEIGHT_FRACTION,
        feet.z,
    );
    mobs.with(|sim| {
        sim.spawn_eye_of_ender(
            launch,
            Vec3::new(f64::from(target.x), f64::from(target.y), f64::from(target.z)),
        )
    });
    Some(crate::effects::WorldEffect::Sound {
        sound: "minecraft:entity.ender_eye.launch".to_owned(),
        category: lodestone_model::SoundCategory::Neutral,
        pos: feet,
        volume: 1.0,
        pitch: 0.33 + (0.5 - 0.33) * sound_roll,
        seed: (sound_roll * 1_000_000.0) as i64,
    })
}

/// Applies a `USE_ITEM`: ordered item-use arms, plus projectile items whose
/// specialized behavior replaces the ordinary path.
///
/// The order below is load-bearing — see `crate::item_use`'s module doc. The
/// launch arm sits first because those projectile items use a disjoint path and
/// cannot race the arms below.
///
/// `food_level` and `invulnerable` are the acting player's values for the two
/// non-item can-eat conditions.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_use_item(
    mobs: &MobHandle,
    effects: &crate::mob_effects::ActiveEffects,
    inventory: &mut PlayerInventory,
    player_pos: Option<(f64, f64, f64)>,
    client_movement: ClientMovement,
    game_mode: GameMode,
    food_level: i32,
    invulnerable: bool,
    hand: u8,
    yaw: f32,
    pitch: f32,
    // The fishing-rod cast/retrieve dispatch needs the caster's own
    // entity id, both to own the bobber (`MobSim::cast_fishing_bobber`'s
    // `owner`) and to find it again on the next click
    // (`MobSim::player_active_bobber`).
    player_entity_id: i32,
) -> UseItemOutcome {
    let native = if hand == 1 {
        crate::inventory::OFFHAND_NATIVE
    } else {
        usize::from(inventory.selected_hotbar_slot())
    };
    let Some(stack) = inventory.native(native) else {
        return UseItemOutcome::Nothing;
    };
    let held = stack.item.to_string();
    let path = stack.item.path().to_owned();
    // Captured before `consume_one` borrows the inventory mutably, and before
    // the stack this reads is gone. A splash or lingering potion carries its
    // identity here and nowhere else on the launch path, so without this the
    // thrown entity has no potion to apply on impact and the whole splash
    // implementation is unreachable from play.
    let thrown_potion = stack.components.potion.and_then(PotionId::from_registry_id);

    if let Some(intent) = launch_intent(&path) {
        // No tracked position means no launch origin, and guessing the origin
        // would put an arrow at the world origin — the same "no data yet, don't
        // guess" gate `apply_attack` uses for knockback direction. Checked here
        // rather than at the top of the function so a *consume* still works before
        // the first movement packet arrives; it needs no position at all.
        let Some((x, y, z)) = player_pos else {
            return UseItemOutcome::Nothing;
        };
        return match intent {
            LaunchIntent::BeginDraw => {
                // The arrow check happens at *release*, not here: vanilla lets a
                // player draw an empty bow (the animation plays) and simply
                // declines to fire. Refusing the draw would also make the release
                // arm unable to tell "no ammunition" from "never drew".
                UseItemOutcome::Draw(BowDraw {
                    started_tick: mobs.with(|sim| sim.tick_count()),
                    yaw,
                    pitch,
                })
            }
            LaunchIntent::InstantThrow {
                projectile,
                power,
                pitch_offset,
            } => {
                if !consume_one(inventory, native, game_mode) {
                    return UseItemOutcome::Nothing;
                }
                let velocity = client_movement.add_to_launch(
                    lodestone_entity::projectile::launch_velocity(
                        f64::from(yaw),
                        f64::from(pitch),
                        pitch_offset,
                        power,
                    ),
                );
                spawn_player_projectile(
                    mobs,
                    projectile,
                    Vec3::new(x, y + EYE_HEIGHT, z),
                    velocity,
                    thrown_potion,
                );
                UseItemOutcome::Nothing
            }
        };
    }

    // Vanilla's own fishing-rod-item use routine: overrides its own item-use routine entirely, exactly like the
    // launch-intent items above, so it sits ahead of the `Consumable`/
    // `Equippable` arms rather than as one of them. A rod already carrying a
    // live bobber reels it in; otherwise it casts a fresh one.
    if path == "fishing_rod" {
        let Some((x, y, z)) = player_pos else {
            return UseItemOutcome::Nothing;
        };
        if let Some(bobber_id) = mobs.with(|sim| sim.player_active_bobber(player_entity_id)) {
            // Vanilla's own fishing-rod-item use routine's "already fishing" arm — reel it in.
            // `FishingRetrieve::rod_damage` is vanilla's own `hurtAndBreak`
            // tier for the rod; this crate models no item durability at all
            // (see the flint-and-steel precedent in `apply_use_item_on`, whose
            // own comment discloses the same gap), so the catch itself lands
            // for real — loot spawned, xp awarded — and only the durability
            // half is the disclosed no-op.
            // The bobber already carries its rod-derived luck. The player's
            // current Luck/Unluck attribute is sampled when the catch is
            // rolled, so expiry before retrieval is observable and no second
            // effect timer is needed in the fishing simulation.
            mobs.with(|sim| {
                sim.retrieve_fishing_bobber(
                    bobber_id,
                    Vec3::new(x, y, z),
                    effects.luck(),
                )
            });
        } else {
            // Vanilla's own fishing-rod-item use routine's cast arm. `luck`/`lure_speed` are `0, 0`
            // No enchantment model reaches this call site yet (see
            // `MobSim::cast_fishing_bobber`'s own doc).
            mobs.with(|sim| {
                sim.cast_fishing_bobber(
                    player_entity_id,
                    Vec3::new(x, y, z),
                    y + EYE_HEIGHT,
                    yaw,
                    pitch,
                    0,
                    0,
                )
            });
        }
        return UseItemOutcome::Nothing;
    }

    // Arm 1: vanilla's own consumable data component → its own start-consuming routine, whose
    // own `canConsume` is vanilla's own can-eat check. A refusal is vanilla's `FAIL` — no use
    // starts, so a full player's right-click on steak does nothing at all, which
    // is the behaviour whose absence is most visible.
    if let Some(food) = crate::item_use::food_for_item(&held) {
        if !crate::item_use::can_eat(food, food_level, invulnerable) {
            return UseItemOutcome::Nothing;
        }
        let now = mobs.with(|sim| sim.tick_count());
        return UseItemOutcome::Consuming(ItemInUse {
            native,
            item: held,
            finish_tick: now + u64::try_from(food.use_ticks.max(0)).unwrap_or(0),
            last_effect_remaining: None,
        });
    }

    // Arm 2: vanilla's own equippable data component gated on `swappable()`. Instantaneous,
    // and it is behind arm 1 for the reason `crate::item_use`'s doc gives — an
    // item that is both eats rather than equips.
    if let Some(swap) = crate::item_use::swap_with_equipment_slot(
        inventory,
        native,
        game_mode == GameMode::Creative,
    ) {
        return UseItemOutcome::Equipped(swap);
    }

    // Arms 3 and 4 (`BLOCKS_ATTACKS`, `KINETIC_WEAPON`) would only
    // `startUsingItem`, and nothing here consumes a raised shield.
    UseItemOutcome::Nothing
}

/// Finishes a consume whose clock ran out — vanilla's own complete-using-item →
/// finish-using-item → consumable-on-consume → food-properties-on-consume chain,
/// which applies the food value and removes one item from the used stack.
///
/// Returns the slot to report and the stack now in it, or `None` when the use is
/// stale: the slot's contents changed under it (a hotbar swap, a container click)
/// or the food is gone. `Option<Option<..>>` rather than a bool so an emptied slot
/// is reported as an *empty* slot rather than as nothing to report — the
/// zero-count-ghost trap.
pub(super) fn finish_consuming(
    inventory: &mut PlayerInventory,
    vitals: &mut PlayerVitals,
    use_in_progress: &ItemInUse,
    game_mode: GameMode,
) -> Option<(usize, Option<ItemStack>)> {
    let still_there = inventory
        .native(use_in_progress.native)
        .is_some_and(|stack| stack.item.to_string() == use_in_progress.item);
    if !still_there {
        return None;
    }
    let food = crate::item_use::food_for_item(&use_in_progress.item)?;
    let mut data = vitals.food();
    data.eat(food.nutrition, food.saturation_modifier);
    vitals.set_food(data);
    if !consume_one(inventory, use_in_progress.native, game_mode) {
        return None;
    }
    Some((
        use_in_progress.native,
        inventory.native(use_in_progress.native).cloned(),
    ))
}

/// Vanilla's own ominous-bottle-amplifier on-consume routine: finishing a drink of
/// `minecraft:ominous_bottle` grants `minecraft:bad_omen` for 120000 ticks
/// and consumes the bottle — the raid-trigger producer
/// (`item_use.rs`'s own disclosed "potions" gap, closed for exactly this one
/// item rather than generally).
///
/// A separate function from [`finish_consuming`] rather than a branch inside
/// it: that function's success arm is deliberately food-only — its own call
/// sites play the burp sound and `item_consume_finished` effect specifically
/// *because* the item was food (see those call sites' own comments) — and an
/// ominous bottle is not food and must not burp. Same `still_there`/
/// `consume_one` shape as [`finish_consuming`], reused rather than
/// restated.
///
/// The item data does not retain the per-stack amplifier roll, so every bottle
/// grants amplifier `0`. That value still satisfies
/// `absorb_raid_omen(0, 0) == 1` and starts a genuine raid when Bad Omen
/// converts; it represents the weakest roll rather than a no-op.
pub(super) fn finish_drinking_ominous_bottle(
    inventory: &mut PlayerInventory,
    effects: &mut crate::mob_effects::ActiveEffects,
    use_in_progress: &ItemInUse,
    game_mode: GameMode,
) -> Option<(usize, Option<ItemStack>)> {
    if use_in_progress.item != "minecraft:ominous_bottle" {
        return None;
    }
    let still_there = inventory
        .native(use_in_progress.native)
        .is_some_and(|stack| stack.item.to_string() == use_in_progress.item);
    if !still_there {
        return None;
    }
    effects.apply("minecraft:bad_omen", 120_000, 0);
    if !consume_one(inventory, use_in_progress.native, game_mode) {
        return None;
    }
    Some((
        use_in_progress.native,
        inventory.native(use_in_progress.native).cloned(),
    ))
}

/// Finishing a drink of `minecraft:potion` applies the complete built-in effect
/// list without scaling. An empty or unsupported potion entry produces no
/// effects.
///
/// Reuses [`crate::mob_effects::potion_splash_effects`] at `scale = 1.0`,
/// `duration_scale = 1.0` rather than re-deriving the list: that function's own
/// `splash_instant_amount`/`splash_timed_duration` are both the identity
/// transform at `scale = 1.0` (`floor(1.0 * x + 0.5) == x` for the non-negative
/// integer `x` every potion table entry is), so direct drinking preserves every
/// amount and duration from the table. `duration_scale` is `1.0` because this
/// build's `ItemComponents` does not model `minecraft:potion_duration_scale`.
///
/// Returns the `(slot, remaining stack)` pair [`finish_consuming`] does, plus
/// the effect list to apply — `None` when the item is not a potion or the use
/// is stale (the slot's contents changed under it), matching every sibling
/// `finish_*` function's `still_there` gate.
pub(super) fn finish_drinking_potion(
    inventory: &mut PlayerInventory,
    use_in_progress: &ItemInUse,
    game_mode: GameMode,
) -> Option<(usize, Option<ItemStack>, Vec<crate::mob_effects::SplashEffect>)> {
    if use_in_progress.item != "minecraft:potion" {
        return None;
    }
    let stack = inventory.native(use_in_progress.native)?;
    if stack.item.to_string() != use_in_progress.item {
        return None;
    }
    let effects = stack
        .components
        .potion
        .and_then(PotionId::from_registry_id)
        .map(|id| crate::mob_effects::potion_splash_effects(id, 1.0, 1.0))
        .unwrap_or_default();
    if !consume_one(inventory, use_in_progress.native, game_mode) {
        return None;
    }
    Some((
        use_in_progress.native,
        inventory.native(use_in_progress.native).cloned(),
        effects,
    ))
}

/// Vanilla's own consumables table's milk-bucket on-consume entry
/// (its own clear-all-status-effects consume effect) — a drunk milk bucket wipes every active status effect.
///
/// Returns the `(slot, remaining stack)` pair plus the ids that were actually
/// active (and are now gone), so the caller can send one
/// `encode_remove_mob_effect` per id rather than guessing which ones changed.
/// An empty vec is a real answer (a player with nothing active drank milk for
/// nothing, exactly like vanilla), not a "did not run" sentinel — matching the
/// water-bottle-control shape this crate's other consume paths already use.
///
/// **Disclosed narrowing**: vanilla's `MilkBucketItem` additionally converts
/// the stack to `minecraft:bucket` (`usingConvertsTo`) rather than consuming it
/// outright; `item_use`'s own module doc already names `usingConvertsTo` as not
/// modelled (a stew leaving a bowl is the same gap), so this reuses
/// [`consume_one`] like every other drink here and empties the stack instead.
/// The effect-clearing half — this function's actual reason to exist — is
/// complete.
pub(super) fn finish_drinking_milk(
    inventory: &mut PlayerInventory,
    effects: &mut crate::mob_effects::ActiveEffects,
    use_in_progress: &ItemInUse,
    game_mode: GameMode,
) -> Option<(usize, Option<ItemStack>, Vec<String>)> {
    if use_in_progress.item != "minecraft:milk_bucket" {
        return None;
    }
    let still_there = inventory
        .native(use_in_progress.native)
        .is_some_and(|stack| stack.item.to_string() == use_in_progress.item);
    if !still_there {
        return None;
    }
    let cleared: Vec<String> = effects
        .active()
        .into_iter()
        .map(|(id, _)| id.to_owned())
        .collect();
    effects.clear();
    if !consume_one(inventory, use_in_progress.native, game_mode) {
        return None;
    }
    Some((
        use_in_progress.native,
        inventory.native(use_in_progress.native).cloned(),
        cleared,
    ))
}

/// Applies a `RELEASE_USE_ITEM` that ends a bow draw: computes the charge, refuses
/// a shot too weak or unarmed, and launches the arrow.
///
/// Returns `true` if an arrow was actually fired, so a caller (and a gate) can
/// tell a released-but-declined draw from a shot.
pub(super) fn apply_release_use_item(
    mobs: &MobHandle,
    inventory: &mut PlayerInventory,
    player_pos: Option<(f64, f64, f64)>,
    client_movement: ClientMovement,
    player_rot: Option<Rotation>,
    game_mode: GameMode,
    draw: BowDraw,
) -> bool {
    use lodestone_entity::projectile::{BOW_ARROW_SPEED, BOW_MIN_POWER, bow_power_for_time};
    let Some((x, y, z)) = player_pos else {
        return false;
    };
    // Ticks, from the server's own 20 TPS counter — never `Instant::now()`, which
    // compiles on wasm32 and then panics at runtime under `panic = "abort"` with no
    // log line. `saturating_sub` because the counter is shared and a draw recorded
    // against a sim that was later reseeded must read as a zero-length draw rather
    // than wrapping to an enormous one.
    let held_ticks = mobs
        .with(|sim| sim.tick_count())
        .saturating_sub(draw.started_tick);
    let power = bow_power_for_time(i32::try_from(held_ticks).unwrap_or(i32::MAX));
    if power < BOW_MIN_POWER {
        return false;
    }
    // Vanilla's own bow-item release-using routine resolves the ammunition *before* checking the power in
    // vanilla; the order is unobservable here because neither has a side effect
    // until both pass.
    let Some(ammo_slot) = find_item_slot(inventory, BOW_AMMUNITION) else {
        return false;
    };
    if !consume_one(inventory, ammo_slot, game_mode) {
        return false;
    }
    let rotation = player_rot.unwrap_or(Rotation {
        yaw: draw.yaw,
        pitch: draw.pitch,
    });
    let velocity = client_movement.add_to_launch(
        lodestone_entity::projectile::launch_velocity(
            f64::from(rotation.yaw),
            f64::from(rotation.pitch),
            0.0,
            power * BOW_ARROW_SPEED,
        ),
    );
    spawn_player_projectile(mobs, "arrow", Vec3::new(x, y + EYE_HEIGHT, z), velocity, None);
    true
}

/// Spawns one player-launched projectile into the live sim, picking the ballistic
/// family from the projectile's own registry path.
///
/// `owner` is `None`: this crate's [`MobSim`] numbers mobs and projectiles in one
/// id space that connected **players** are not part of (their ids come from the
/// `PlayerRegistry`), so there is no mob id to exclude — and players are not
/// impact candidates either, so a player cannot be hit by their own arrow
/// regardless. Passing a player entity id here would silently exclude whichever
/// *mob* happened to share that number, which is worse than passing nothing.
///
/// `potion` is the thrown stack's validated `minecraft:potion` identity, and is what
/// [`MobSim::resolve_potion_splash`] later reads to decide which effects the
/// impact applies. It is `None` for every projectile that is not a splash or
/// lingering potion, and also for a potion stack carrying no potion component —
/// a water bottle, which correctly applies nothing.
pub(super) fn spawn_player_projectile(
    mobs: &MobHandle,
    projectile: &str,
    origin: Vec3,
    velocity: Vec3,
    potion: Option<PotionId>,
) {
    use lodestone_entity::projectile::Projectile;
    let Ok(key) = lodestone_model::ResourceKey::new("minecraft", projectile) else {
        return;
    };
    // The two families disagree on gravity, drag *and* step order — see
    // `lodestone_entity::projectile`'s module doc. A trident integrates as an
    // arrow despite being thrown.
    let ballistic = match projectile {
        "arrow" | "spectral_arrow" | "trident" => Projectile::arrow(origin, velocity),
        _ => Projectile::throwable(origin, velocity),
    };
    // Only the two potion kinds take the potion-carrying spawn; everything else
    // would record a `potion` nothing reads. Splitting on the projectile name
    // rather than on `potion.is_some()` keeps a mis-set component from turning
    // a snowball into a splash.
    match projectile {
        "splash_potion" | "lingering_potion" => mobs.with(|sim| {
            sim.spawn_potion_projectile_from(key.clone(), ballistic, None, potion);
        }),
        _ => mobs.with(|sim| {
            sim.spawn_projectile_from(key.clone(), ballistic, None);
        }),
    }
}

/// The first native slot holding `path`, if any.
pub(super) fn find_item_slot(inventory: &PlayerInventory, path: &str) -> Option<usize> {
    (0..crate::inventory::PLAYER_NATIVE_SIZE).find(|&i| {
        inventory
            .native(i)
            .is_some_and(|stack| stack.item.path() == path)
    })
}

/// Removes one item from native slot `native`, clearing the slot when the stack
/// empties. A creative-mode player consumes nothing but still succeeds.
///
/// Returns whether the launch may proceed — `false` only when the slot turned out
/// to be empty, which a caller reads as "no ammunition".
pub(super) fn consume_one(inventory: &mut PlayerInventory, native: usize, game_mode: GameMode) -> bool {
    let Some(stack) = inventory.native(native) else {
        return false;
    };
    if game_mode == GameMode::Creative {
        return true;
    }
    let mut stack = stack.clone();
    if stack.count <= 1 {
        inventory.set_native(native, None);
    } else {
        stack.count -= 1;
        inventory.set_native(native, Some(stack));
    }
    true
}

#[cfg(test)]
mod eye_of_ender_throw_tests {
    use super::*;
    use crate::dimension::Dimension;

    fn eyes_in(stack: u32) -> PlayerInventory {
        let mut inventory = PlayerInventory::new();
        inventory.set_native(0, Some(ItemStack::new("minecraft:ender_eye".parse().unwrap(), stack)));
        inventory
    }

    fn air(_x: i32, _y: i32, _z: i32) -> StateId {
        crate::chunk::air_state()
    }

    /// A locator that always answers with a stronghold 400 blocks east.
    fn east_stronghold(_from: BlockPos) -> Option<BlockPos> {
        Some(BlockPos::new(400, 0, 0))
    }

    const FEET: Vec3 = Vec3::new(0.5, 64.0, 0.5);

    #[test]
    fn a_throw_spawns_an_eye_consumes_one_and_plays_the_launch_sound() {
        let mobs = MobHandle::default();
        let mut inventory = eyes_in(3);
        let sound = launch_eye_of_ender(
            &mobs, &mut inventory, 0, GameMode::Survival, FEET, 0.0, 0.0,
            Dimension::Overworld, &air, &east_stronghold, 0.5,
        );
        assert_eq!(mobs.with(|sim| sim.eye_count()), 1);
        assert_eq!(inventory.native(0).map(|stack| stack.count), Some(2));
        // The launch pitch is lerp(roll, 0.33, 0.5): 0.33 + 0.17 * 0.5 = 0.415.
        match sound {
            Some(crate::effects::WorldEffect::Sound { sound, pitch, volume, .. }) => {
                assert_eq!(sound, "minecraft:entity.ender_eye.launch");
                assert!((pitch - 0.415).abs() < 1e-6, "{pitch}");
                assert!((volume - 1.0).abs() < f32::EPSILON);
            }
            other => panic!("expected the launch sound, got {other:?}"),
        }
    }

    /// The eye leaves from half the 1.8-block standing height (64.9), and one
    /// tick later it has not moved (the first tick moves by the zero launch
    /// velocity) but has speed 0.0025 * 12 along +x toward the clamped target.
    #[test]
    fn the_eye_launches_from_mid_body_and_heads_for_the_stronghold() {
        let mobs = MobHandle::default();
        let mut inventory = eyes_in(1);
        launch_eye_of_ender(
            &mobs, &mut inventory, 0, GameMode::Survival, FEET, 0.0, 0.0,
            Dimension::Overworld, &air, &east_stronghold, 0.0,
        )
        .expect("thrown");
        assert_eq!(inventory.native(0), None);
        let snaps = mobs.with(|sim| sim.snapshots());
        let eye = snaps
            .iter()
            .find(|s| s.entity_type.to_string() == "minecraft:eye_of_ender")
            .expect("the eye streams");
        assert!((eye.position.y - 64.9).abs() < 1e-9);
        mobs.with(|sim| sim.tick_eyes());
        let (position, velocity) = mobs.with(|sim| {
            let id = sim.snapshots().iter().find(|s| s.entity_type.to_string() == "minecraft:eye_of_ender").unwrap().id;
            sim.eye_motion(id).unwrap()
        });
        assert!((position.x - 0.5).abs() < 1e-12);
        // Target (400, 0, 0) from (0.5, 64.9, 0.5) is 399.5 east, 0.5 north of
        // the launch: clamped to 12 along that bearing, so vx ~= 0.03.
        assert!((velocity.x - 0.03).abs() < 1e-4, "{velocity:?}");
        assert!((velocity.y - 0.015).abs() < 1e-12);
    }

    #[test]
    fn creative_throws_keep_the_stack() {
        let mobs = MobHandle::default();
        let mut inventory = eyes_in(1);
        let thrown = launch_eye_of_ender(
            &mobs, &mut inventory, 0, GameMode::Creative, FEET, 0.0, 0.0,
            Dimension::Overworld, &air, &east_stronghold, 0.0,
        );
        assert!(thrown.is_some());
        assert_eq!(inventory.native(0).map(|stack| stack.count), Some(1));
        assert_eq!(mobs.with(|sim| sim.eye_count()), 1);
    }

    #[test]
    fn no_throw_outside_the_overworld_or_without_a_stronghold() {
        for dimension in [Dimension::Nether, Dimension::End] {
            let mobs = MobHandle::default();
            let mut inventory = eyes_in(1);
            let thrown = launch_eye_of_ender(
                &mobs, &mut inventory, 0, GameMode::Survival, FEET, 0.0, 0.0,
                dimension, &air, &east_stronghold, 0.0,
            );
            assert!(thrown.is_none());
            assert_eq!(inventory.native(0).map(|stack| stack.count), Some(1));
            assert_eq!(mobs.with(|sim| sim.eye_count()), 0);
        }
        let mobs = MobHandle::default();
        let mut inventory = eyes_in(1);
        let thrown = launch_eye_of_ender(
            &mobs, &mut inventory, 0, GameMode::Survival, FEET, 0.0, 0.0,
            Dimension::Overworld, &air, &|_| None, 0.0,
        );
        assert!(thrown.is_none());
        assert_eq!(inventory.native(0).map(|stack| stack.count), Some(1));
        assert_eq!(mobs.with(|sim| sim.eye_count()), 0);
    }

    /// Looking straight down at a portal frame leaves the eye to the
    /// frame-filling arm; the same look at open air throws (the control).
    #[test]
    fn aiming_at_an_end_portal_frame_throws_nothing() {
        let frame = crate::portal::end_portal_frame_state(Direction::North, false);
        let frame_below = move |x: i32, y: i32, z: i32| {
            if (x, y, z) == (0, 63, 0) { frame } else { crate::chunk::air_state() }
        };
        let mobs = MobHandle::default();
        let mut inventory = eyes_in(1);
        let thrown = launch_eye_of_ender(
            &mobs, &mut inventory, 0, GameMode::Survival, FEET, 0.0, 90.0,
            Dimension::Overworld, &frame_below, &east_stronghold, 0.0,
        );
        assert!(thrown.is_none());
        assert_eq!(inventory.native(0).map(|stack| stack.count), Some(1));
        let control = launch_eye_of_ender(
            &mobs, &mut inventory, 0, GameMode::Survival, FEET, 0.0, 90.0,
            Dimension::Overworld, &air, &east_stronghold, 0.0,
        );
        assert!(control.is_some(), "the same look at air must throw");
    }

    /// Hand-worked squared distances from (0, 64, 0) to each chunk centre at
    /// y = 32: (10, 0) -> 168^2 + 32^2 + 8^2 = 29312, (-3, 5) -> 40^2 + 32^2 +
    /// 88^2 = 10368, (0, 20) -> 8^2 + 32^2 + 328^2 = 108672. The middle one
    /// wins and is reported at its chunk's minimum corner, y = 0.
    #[test]
    fn the_nearest_ring_start_is_reported_at_its_chunk_corner() {
        let origins = [(10, 0), (-3, 5), (0, 20)];
        let found = crate::chunk::nearest_ring_start(&origins, BlockPos::new(0, 64, 0));
        assert_eq!(found, Some(BlockPos::new(-48, 0, 80)));
        assert_eq!(crate::chunk::nearest_ring_start(&[], BlockPos::new(0, 64, 0)), None);
    }

    #[test]
    fn other_items_are_ignored() {
        let mobs = MobHandle::default();
        let mut inventory = PlayerInventory::new();
        inventory.set_native(0, Some(ItemStack::new("minecraft:ender_pearl".parse().unwrap(), 1)));
        assert!(launch_eye_of_ender(
            &mobs, &mut inventory, 0, GameMode::Survival, FEET, 0.0, 0.0,
            Dimension::Overworld, &air, &east_stronghold, 0.0,
        )
        .is_none());
    }
}
