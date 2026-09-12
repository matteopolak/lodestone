//! Accessors, interaction state, and snapshot projection for one simulated mob.
//!
//! [`super::SimMob`] stores the state; this module keeps its public surface and
//! wire projection together while the tick orchestration remains on
//! [`super::MobSim`].

use super::*;

impl<'w> SimMob<'w> {
    /// The entity id assigned at spawn.
    #[must_use]
    pub fn id(&self) -> i32 {
        self.id
    }

    /// Adds a prioritised goal (higher priority preempts lower on shared flags),
    /// returning `&mut self` so goals can be chained at spawn.
    pub fn add_goal(&mut self, priority: i32, goal: Box<dyn Goal>) -> &mut Self {
        self.goals.add(priority, goal);
        self
    }

    /// Sets the mob's current attack target.
    pub fn set_attack_target(&mut self, target: Option<Vec3>) {
        self.mob.set_attack_target(target);
    }

    /// Puts this animal into love mode for
    /// [`LOVE_TICKS`](lodestone_entity::ai::navigating_mob::LOVE_TICKS)
    /// (vanilla's own animal "set in love" call) — what feeding
    /// it a breeding item does. [`MobSim::tick`]'s partner search only
    /// considers mobs in this state.
    pub fn set_in_love(&mut self) -> &mut Self {
        self.mob.set_in_love();
        self
    }

    /// Whether this animal is currently in love mode.
    #[must_use]
    pub fn is_in_love(&self) -> bool {
        self.mob.is_in_love()
    }

    /// Remaining love-mode ticks (vanilla's own love-time getter).
    #[must_use]
    pub fn love_time(&self) -> i32 {
        self.mob.love_time()
    }

    /// The mob's age timer: negative while a baby (counting up to `0`),
    /// positive as the post-breeding parent cooldown (counting down to `0`).
    #[must_use]
    pub fn age(&self) -> i32 {
        self.mob.age()
    }

    /// Sets the age timer — e.g.
    /// [`BABY_START_AGE`](lodestone_entity::ai::navigating_mob::BABY_START_AGE)
    /// to spawn this mob as a baby.
    ///
    /// **Also re-derives the hitbox and movement step when this crosses the
    /// baby/adult boundary** — vanilla's own ageable-mob age setter unconditionally
    /// refreshes dimensions,
    /// and until this only [`spawn_species`](Self) ever computed
    /// [`species_shape`]: a mob bred into babyhood, or one growing up, kept
    /// its spawn-time adult box and adult step speed forever. Gated on the
    /// boundary rather than run on every call, since a baby's per-tick
    /// countdown from [`BABY_START_AGE`](lodestone_entity::ai::navigating_mob::BABY_START_AGE)
    /// to `0` would otherwise re-resolve both twenty-four thousand times for
    /// no observable change.
    pub fn set_age(&mut self, age: i32) -> &mut Self {
        let was_baby = self.mob.is_baby();
        self.mob.set_age(age);
        let is_baby = self.mob.is_baby();
        if is_baby != was_baby {
            let attrs = default_attributes(&self.entity_type).unwrap_or_else(AttributeMap::new);
            let mut shape = species_shape(&self.entity_type, &attrs, is_baby);
            // `can_open_doors` for the zombie family is a spawn-time RNG roll
            // (see `MobSim::spawn_species`), not a function of size — preserve
            // it across this refresh rather than re-deriving the static
            // per-species default, which would silently reset a zombie that
            // rolled `true` back to `false` the moment it grows up.
            shape.can_open_doors = self.mob.shape().can_open_doors;
            self.mob.set_shape(shape);
            let base_speed = attr(&attrs, "movement_speed");
            let multiplier = if is_baby {
                baby_speed_multiplier(&self.entity_type)
            } else {
                1.0
            };
            self.mob.set_step_per_tick(ai_ground_speed(base_speed * multiplier));
        }
        self
    }

    /// This mob's current per-tick movement step — reflects
    /// [`baby_speed_multiplier`] once [`set_age`](Self::set_age) has crossed
    /// the baby/adult boundary.
    #[must_use]
    pub fn step_per_tick(&self) -> f64 {
        self.mob.step_per_tick()
    }

    /// Whether this mob is a baby (`age < 0`), which gates following a parent
    /// and excludes it from breeding.
    #[must_use]
    pub fn is_baby(&self) -> bool {
        self.mob.is_baby()
    }

    /// Whether this mob is inside its post-damage panic window
    /// ([`PANIC_DAMAGE_TICKS`](lodestone_entity::ai::navigating_mob::PANIC_DAMAGE_TICKS)).
    #[must_use]
    pub fn is_panicking(&self) -> bool {
        self.mob.is_panicking()
    }

    /// Vanilla's own "is scared" check — whether this mob is currently curled up
    /// (halved incoming damage; see [`apply_damage`](Self::apply_damage)).
    /// Always `false` for a non-armadillo species, where the backing field
    /// never leaves `0`.
    #[must_use]
    pub fn armadillo_is_scared(&self) -> bool {
        self.armadillo_danger_ticks > 0
    }

    /// Vanilla's own "is playing dead" check — whether this mob is currently in its
    /// play-dead window (see [`apply_damage`](Self::apply_damage)'s own
    /// axolotl arm for the trigger). Always `false` for a non-axolotl
    /// species, where the backing field never leaves `0`.
    #[must_use]
    pub fn axolotl_is_playing_dead(&self) -> bool {
        self.axolotl_play_dead_ticks > 0
    }

    /// Vanilla's own "is camel sitting" check. Always `false` for a non-camel species,
    /// where the backing field never leaves its default.
    #[must_use]
    pub fn camel_is_sitting(&self) -> bool {
        self.camel_sitting
    }

    /// Vanilla's own "is dashing" check — see the `camel_dash_cooldown` field's own doc
    /// for the disclosed "fixed minimum duration, not a real
    /// landing-triggered reset" narrowing this derives from. Always `false`
    /// for a non-camel species, where the backing field never leaves `0`.
    #[must_use]
    pub fn camel_is_dashing(&self) -> bool {
        self.camel_dash_cooldown > CAMEL_DASH_COOLDOWN_TICKS - CAMEL_DASH_MINIMUM_DURATION_TICKS
    }

    /// The position of the note block this allay most recently heard and
    /// still remembers (vanilla's own liked-note-block-position memory), if the
    /// cooldown hasn't lapsed — see the backing field's own doc. `None` for
    /// every non-allay species.
    #[must_use]
    pub fn allay_liked_noteblock(&self) -> Option<Vec3> {
        self.allay_liked_noteblock.and_then(|(pos, ticks)| (ticks > 0).then_some(pos))
    }

    /// How many items this allay is currently carrying beyond the one held
    /// in its hand — see the backing field's own doc. `0` for every
    /// non-allay species.
    #[must_use]
    pub fn allay_inventory_count(&self) -> u32 {
        self.allay_inventory_count
    }

    /// The position of whatever most recently hurt this mob, while inside the
    /// retaliation window
    /// ([`LAST_HURT_BY_TICKS`](lodestone_entity::ai::navigating_mob::LAST_HURT_BY_TICKS)).
    #[must_use]
    pub fn last_hurt_by(&self) -> Option<Vec3> {
        self.mob.last_hurt_by()
    }

    /// Whether the mob's feet cell holds water, read from the world (never
    /// injected) — the input for floating behavior.
    #[must_use]
    pub fn in_water(&self) -> bool {
        self.mob.in_water()
    }

    /// Whether the mob's feet cell holds lava.
    #[must_use]
    pub fn in_lava(&self) -> bool {
        self.mob.in_lava()
    }

    /// The nearest-player position [`MobSim::tick`] last fed this mob, if any.
    /// `None` when no player is known — including when nothing has ever called
    /// [`MobSim::set_players`], which is still the case in production; see that
    /// method's doc comment.
    #[must_use]
    pub fn nearest_player(&self) -> Option<Vec3> {
        self.mob.nearest_player()
    }

    /// The tempting-entity position [`MobSim::tick`] last fed this mob.
    #[must_use]
    pub fn temptation(&self) -> Option<Vec3> {
        self.mob.temptation()
    }

    /// The threat position [`MobSim::tick`] last fed this mob, from
    /// [`avoided_species`]'s table.
    #[must_use]
    pub fn avoid_threat(&self) -> Option<Vec3> {
        self.mob.avoid_threat()
    }

    /// The nearest-adult position [`MobSim::tick`] last fed this mob, which is
    /// the target for parent-following behavior. Always `None` for an adult.
    #[must_use]
    pub fn parent_candidate(&self) -> Option<Vec3> {
        self.mob.parent_position()
    }

    /// Who owns this mob, if anyone. `None` for a wild (untamed) mob.
    #[must_use]
    pub fn owner(&self) -> Option<MobOwner> {
        self.owner
    }

    /// The **mob** id of this mob's owner, if it is owned by a mob. `None` both
    /// for a wild mob and for one owned by a *player* — read
    /// [`owner`](Self::owner) when the difference matters.
    #[must_use]
    pub fn owner_id(&self) -> Option<i32> {
        match self.owner {
            Some(MobOwner::Mob(id)) => Some(id),
            _ => None,
        }
    }

    /// The uuid of this mob's owner, if it is owned by a player.
    #[must_use]
    pub fn owner_uuid(&self) -> Option<Uuid> {
        match self.owner {
            Some(MobOwner::Player(uuid)) => Some(uuid),
            _ => None,
        }
    }

    /// Sets this mob's owner id (the mob-to-mob flavour of
    /// [`set_owner`](Self::set_owner)).
    pub fn set_owner_id(&mut self, owner_id: Option<i32>) -> &mut Self {
        self.set_owner(owner_id.map(MobOwner::Mob))
    }

    /// Sets this mob's owner — vanilla's own tamed-animal owner setter.
    ///
    /// **Does not set the tame flag**, and the asymmetry is vanilla's:
    /// vanilla's own owner-reference setter sets the owner-uuid metadata field and *then* calls
    /// the tame-flags setter with the tame bit set but the "gift particles" bit clear, two separate pieces of state.
    /// [`tame`](Self::tame) is the call that
    /// does both, and is what a taming interaction should use.
    pub fn set_owner(&mut self, owner: Option<MobOwner>) -> &mut Self {
        self.owner = owner;
        self
    }

    /// Resets this mob's own [`shoulder_dismount_ticks`](Self::shoulder_dismount_ticks)
    /// counter — called with `0` the tick a dismounted parrot respawns, the
    /// same way vanilla's `rideCooldownCounter` starts at `0` on a fresh
    /// entity.
    pub fn set_shoulder_dismount_ticks(&mut self, ticks: i32) -> &mut Self {
        self.shoulder_dismount_ticks = ticks;
        self
    }

    /// What a lead currently ties this mob to, if anything.
    #[must_use]
    pub fn leash_holder(&self) -> Option<LeashHolder> {
        self.leash_holder
    }

    /// Whether a lead is currently attached — vanilla's own "is leashed" check,
    /// which additionally requires a non-null leash holder; this sim has no
    /// "has leash data but no resolved holder" state (see the field's own
    /// doc comment), so `Some` and "leashed" coincide exactly.
    #[must_use]
    pub fn is_leashed(&self) -> bool {
        self.leash_holder.is_some()
    }

    /// Directly sets the leash holder, bypassing [`MobSim::try_leash`]'s
    /// distance/species gating — for a host that has already decided (e.g.
    /// restoring a save, or [`MobSim::try_leash_to_fence`]'s re-parent of an
    /// already-leashed mob onto a fresh knot).
    pub fn set_leash_holder(&mut self, holder: Option<LeashHolder>) -> &mut Self {
        self.leash_holder = holder;
        self
    }

    /// Whether this mob is tame — vanilla's own "is tame" check.
    ///
    /// Distinct from [`owner_position`](Self::owner_position) being `Some`: a
    /// tamed pet whose owner is offline is still tame.
    #[must_use]
    pub fn is_tame(&self) -> bool {
        self.tame
    }

    /// Tames this mob to `owner` — vanilla's own tame-to-player call, which is
    /// the tame-flags setter with both bits set, plus the owner setter.
    pub fn tame(&mut self, owner: MobOwner) -> &mut Self {
        self.owner = Some(owner);
        self.tame = true;
        self.mob.set_tame(true);
        self
    }

    /// Whether the owner has told this mob to sit — vanilla's own
    /// "is ordered to sit" check, the persisted intent.
    #[must_use]
    pub fn is_ordered_to_sit(&self) -> bool {
        self.ordered_to_sit
    }

    /// Sets the sitting order — vanilla's own "set ordered to sit" call.
    ///
    /// Pushes straight through to the [`NavigatingMob`] as well as recording it
    /// here, so `SitWhenOrderedToGoal` sees the order on the *same* tick rather
    /// than one tick late. Every other perception input is refreshed by
    /// [`MobSim::feed_perception`], but an order given by an interaction arrives
    /// between ticks and a one-tick lag is visible as a pet that ignores the
    /// first click.
    pub fn set_ordered_to_sit(&mut self, ordered_to_sit: bool) -> &mut Self {
        self.ordered_to_sit = ordered_to_sit;
        self.mob.set_ordered_to_sit(ordered_to_sit);
        self
    }

    /// Whether this mob is currently in the sitting **pose** —
    /// `SitWhenOrderedToGoal`'s observable output, which is what the `0x01`
    /// bit of vanilla's shared entity-flags metadata field carries. Read this to answer "did the goal run",
    /// and [`is_ordered_to_sit`](Self::is_ordered_to_sit) to answer "was it
    /// told to".
    #[must_use]
    pub fn is_in_sitting_pose(&self) -> bool {
        self.mob.is_in_sitting_pose()
    }

    /// Whether this mob is part of an active pillager patrol — vanilla's own
    /// patrolling-monster "is patrolling" check. Kept only on the [`NavigatingMob`]
    /// (unlike [`tame`](Self::tame)/[`owner`](Self::owner)): nothing outside the
    /// AI seam and [`MobSim`]'s own patrol census reads it, so there is no
    /// second host-side record to keep in sync.
    #[must_use]
    pub fn is_patrolling(&self) -> bool {
        self.mob.is_patrolling()
    }

    /// Whether this mob leads its patrol — vanilla's own
    /// patrolling-monster "is patrol leader" check.
    #[must_use]
    pub fn is_patrol_leader(&self) -> bool {
        self.mob.is_patrol_leader()
    }

    /// This mob's own current long-distance patrol waypoint — vanilla's own
    /// patrolling-monster patrol-target getter.
    #[must_use]
    pub fn patrol_target(&self) -> Option<Vec3> {
        self.mob.patrol_target()
    }

    /// Marks this mob as patrolling (or not) — vanilla's own
    /// patrolling-monster "set patrolling" call.
    pub fn set_patrolling(&mut self, patrolling: bool) -> &mut Self {
        self.mob.set_patrolling(patrolling);
        self
    }

    /// Marks this mob as its patrol's leader (or not) — vanilla's own
    /// patrolling-monster "set patrol leader" call. Does not also set
    /// [`patrolling`](Self::set_patrolling); see [`NavigatingMob::set_patrol_leader`]'s
    /// own doc comment for why the two are separate calls here.
    pub fn set_patrol_leader(&mut self, leader: bool) -> &mut Self {
        self.mob.set_patrol_leader(leader);
        self
    }

    /// Sets this mob's own long-distance patrol waypoint — vanilla's own
    /// patrolling-monster "set patrol target"/"find patrol target" calls.
    pub fn set_patrol_target(&mut self, target: Option<Vec3>) -> &mut Self {
        self.mob.set_patrol_target(target);
        self
    }

    /// Feeds a non-leader the patrol group's shared waypoint, as
    /// [`MobSim`]'s own per-tick census resolves it. See
    /// [`MobController::patrol_group_target`]'s own doc comment for why this
    /// exists.
    pub fn set_patrol_group_target(&mut self, target: Option<Vec3>) -> &mut Self {
        self.mob.set_patrol_group_target(target);
        self
    }

    /// Vanilla's own horse-family temper getter — how close this horse is to accepting a
    /// rider. Always `0` outside the horse family.
    #[must_use]
    pub fn temper(&self) -> i32 {
        self.temper
    }

    /// Vanilla's own horse-family temper setter, clamped to `0..=max` by the caller. Exists so a
    /// gate can stage a horse at a chosen temper instead of feeding it 34 times.
    pub fn set_temper(&mut self, temper: i32) -> &mut Self {
        self.temper = temper;
        self
    }

    /// The position of this mob's owner as the [`MobController`] seam reports
    /// it — what [`MobSim::tick`]'s feed last resolved from
    /// [`owner_id`](Self::owner_id). `None` until the feed has run, and for a
    /// wild mob.
    #[must_use]
    pub fn owner_position(&self) -> Option<Vec3> {
        self.mob.owner_position()
    }

    /// Teleports this mob directly to `pos` (the instant-relocation primitive) —
    /// the host command the enderman's damage-triggered
    /// teleport and gaze-triggered "teleport towards" reduce to. Rewrites
    /// position immediately and abandons any in-progress path (vanilla's own
    /// generic teleport-to call).
    pub fn teleport_to(&mut self, pos: Vec3) -> &mut Self {
        self.mob.teleport_to(pos);
        self
    }

    /// Records a self-inflicted damage request (the self-damage primitive) — the
    /// bee's sting self-destruct. Drained and
    /// applied by [`MobSim::tick`] through the normal damage pipeline.
    pub fn damage_self(&mut self, amount: f32) -> &mut Self {
        self.mob.damage_self(amount);
        self
    }

    /// The mob's current attack-target *position* (the point its attack
    /// behavior chases), as distinct from
    /// [`attack_target_id`](SimMob::attack_target_id)'s entity identity. This
    /// is the state retaliation writes when the mob is attacked.
    #[must_use]
    pub fn attack_target(&self) -> Option<Vec3> {
        self.mob.attack_target()
    }

    /// Whether a goal has this mob holding jump this tick — the observable
    /// effect of its water-escape behavior.
    #[must_use]
    pub fn is_jumping(&self) -> bool {
        self.mob.is_jumping()
    }

    /// The last position a goal asked this mob to look at, if any — the
    /// observable effect of `LookAtPlayerGoal`. Distinct from
    /// [`head_yaw`](SimMob::head_yaw), which is the derived angle; this is the
    /// target the goal actually chose, so a test can assert *what* the mob
    /// turned toward rather than merely that some angle changed.
    #[must_use]
    pub fn facing(&self) -> Option<Vec3> {
        self.mob.facing()
    }

    /// `no_action_time` **as the goals see it**, through the
    /// [`MobController`] seam.
    ///
    /// Deliberately separate from [`no_action_time`](SimMob::no_action_time),
    /// which reads the sim's own record. The two must stay equal: the sim
    /// increments its record every tick and goals must observe that value
    /// through the controller seam rather than the trait default `0`.
    /// Keeping both readable is what lets a test assert the equality rather
    /// than assume it.
    #[must_use]
    pub fn mob_no_action_time(&self) -> i32 {
        MobController::no_action_time(&self.mob)
    }

    /// How many goals are installed on this mob. Used to assert a
    /// [`MobSim::tick`]-spawned child inherited a goal set rather than arriving
    /// inert.
    #[must_use]
    pub fn goal_count(&self) -> usize {
        self.goals.len()
    }

    /// Marks the mob ignited, forcing a
    /// creeper's swell direction to climb every tick regardless of
    /// proximity check. A no-op for a mob whose [`NavigatingMob`] never has anything
    /// else move its swell direction off `-1` (every non-creeper species).
    pub fn ignite(&mut self) -> &mut Self {
        self.mob.ignite();
        self
    }

    /// Whether this mob is currently ignited. See [`ignite`](Self::ignite).
    #[must_use]
    pub fn is_ignited(&self) -> bool {
        self.mob.is_ignited()
    }

    /// The current fuse counter (vanilla's own creeper fuse field), `0..=MAX_SWELL`
    /// for a creeper; permanently `0` for a species nothing ever moves off
    /// [`swell_dir`](Self::swell_dir)'s `-1` default.
    #[must_use]
    pub fn swell(&self) -> i32 {
        self.mob.swell()
    }

    /// The mob's current swell direction (vanilla's own swell-direction getter).
    #[must_use]
    pub fn swell_dir(&self) -> i32 {
        self.mob.swell_dir()
    }

    /// Sets which live mob (by id) this mob's connecting melee attacks damage.
    /// The goal/navigation seam only ever deals in positions
    /// ([`set_attack_target`](Self::set_attack_target)); this is the identity
    /// [`MobSim::tick`] needs to resolve a strike into an actual
    /// [`apply_damage`](Self::apply_damage) call on the right mob.
    pub fn set_attack_target_id(&mut self, target_id: Option<i32>) -> &mut Self {
        self.attack_target_id = target_id;
        self
    }

    /// The id of the mob this one's connecting attacks currently damage, if set.
    #[must_use]
    pub fn attack_target_id(&self) -> Option<i32> {
        self.attack_target_id
    }

    /// Current health. Reaches `0.0` (never negative) when the mob has taken
    /// lethal damage; [`MobSim::tick`] removes a mob whose health is `0.0` at
    /// the end of the tick that landed the killing blow.
    #[must_use]
    pub fn health(&self) -> f32 {
        self.health
    }

    /// Overrides current health (e.g. to stage a near-death mob in a test).
    /// Clamped to `>= 0.0`.
    pub fn set_health(&mut self, health: f32) -> &mut Self {
        self.health = health.max(0.0);
        self
    }

    /// The `minecraft:max_health` attribute resolved at spawn.
    #[must_use]
    pub fn max_health(&self) -> f32 {
        self.max_health
    }

    /// Vanilla's own generic heal call: raises health toward
    /// [`max_health`](Self::max_health), never past it.
    pub fn heal(&mut self, amount: f32) -> &mut Self {
        self.health = (self.health + amount).min(self.max_health);
        self
    }

    /// Overrides the raw melee damage this mob's attacks deal, in place of the
    /// type's `ATTACK_DAMAGE` default resolved at spawn.
    pub fn set_attack_damage(&mut self, attack_damage: f32) -> &mut Self {
        self.attack_damage = attack_damage;
        self
    }

    /// The raw melee damage this mob's attacks currently deal.
    #[must_use]
    pub fn attack_damage(&self) -> f32 {
        self.attack_damage
    }

    /// Overrides this mob's defensive state (armour/toughness/absorption) in
    /// place of the type's defaults resolved at spawn.
    pub fn set_defenses(&mut self, defenses: Defenses) -> &mut Self {
        self.defenses = defenses;
        self
    }

    /// This mob's current defensive state.
    #[must_use]
    pub fn defenses(&self) -> &Defenses {
        &self.defenses
    }

    /// Overrides this mob's `minecraft:knockback_resistance` value in place
    /// of the type's default resolved at spawn.
    pub fn set_knockback_resistance(&mut self, knockback_resistance: f64) -> &mut Self {
        self.knockback_resistance = knockback_resistance;
        self
    }

    /// This mob's current `minecraft:knockback_resistance` value.
    #[must_use]
    pub fn knockback_resistance(&self) -> f64 {
        self.knockback_resistance
    }

    /// Applies a velocity impulse to this mob — see
    /// [`NavigatingMob::apply_knockback`] for the exact one-tick-displacement
    /// mechanic this forwards to.
    pub fn apply_knockback(&mut self, impulse: Vec3) {
        self.mob.apply_knockback(impulse);
    }

    /// Runs the full vanilla hit pipeline against this mob for one incoming
    /// hit of `raw_damage`: the invulnerability-frame gate
    /// ([`HurtCooldown::on_hurt`]), then armour/resistance/enchantment/
    /// absorption reduction ([`apply_reductions`](lodestone_entity::apply_reductions)),
    /// then subtracts the result from [`health`](Self::health) (floored at
    /// `0.0`). A hit fully inside the i-frame window and no stronger than the
    /// one that opened it is ignored entirely, exactly as vanilla drops a
    /// weaker follow-up hit.
    ///
    /// Returns the damage that actually reached health (`0.0` if the hit was
    /// ignored, if it was fully absorbed, or if the mob was already dead).
    pub fn apply_damage(&mut self, raw_damage: f32, flags: DamageFlags) -> f32 {
        if self.health <= 0.0 {
            return 0.0;
        }
        // Vanilla's own "is invulnerable to" check: a digging-or-emerging gate blocks every hit
        // except one tagged as bypassing invulnerability entirely (void,
        // `/kill`, and similarly out-of-world sources). That carve-out is not
        // modelled as a `DamageFlags` bit here (see
        // `lodestone_entity::damage::DamageFlags`'s field list — it has no
        // `bypasses_invulnerability`), so this is a narrower, disclosed
        // invulnerability than vanilla's: a hit that should still land during
        // emerge is blocked too. Only the `Emerging` half is modelled — see
        // `crate::mobs::warden`'s module doc for the `Digging` half this
        // crate does not build.
        if self.entity_type.path() == "warden" && self.warden_emerge_ticks > 0 {
            return 0.0;
        }
        // Vanilla's own armadillo hurt-handler override, which runs *before* the
        // invulnerability-frame check below (it wraps the generic hurt
        // handler, not the "actually hurt" one) — a curled-up armadillo halves the raw hit before
        // anything else sees it, including whether the hit even breaks
        // through i-frames.
        let raw_damage = if self.entity_type.path() == "armadillo" && self.armadillo_danger_ticks > 0 {
            ((raw_damage - 1.0) / 2.0).max(0.0)
        } else {
            raw_damage
        };
        // Vanilla's own axolotl hurt-handler — runs before the generic hurt
        // handler's own
        // invulnerability-frame gate, exactly like the armadillo halving
        // above, so this reads `self.health`/`raw_damage` directly rather
        // than the post-`on_hurt` `amount`. **Disclosed narrowing**: real
        // vanilla additionally requires a live source/direct entity
        // — this seam has no attacker-identity input to gate on, the same
        // simplification `armadillo_danger_ticks`'s own doc already
        // discloses for the same missing attacker-identity input.
        if self.entity_type.path() == "axolotl"
            && self.axolotl_play_dead_ticks <= 0
            && self.in_water()
            && raw_damage < self.health
        {
            let health_ratio = self.health / self.max_health;
            let (roll_a, roll_b) =
                axolotl_play_dead_roll(self.id as u64, self.health.to_bits(), raw_damage.to_bits());
            if roll_a == 0 && ((roll_b as f32) < raw_damage || health_ratio < 0.5) {
                self.axolotl_play_dead_ticks = AXOLOTL_PLAY_DEAD_TICKS;
            }
        }
        let amount = match self.hurt_cooldown.on_hurt(raw_damage, flags) {
            HurtDecision::Ignored => return 0.0,
            HurtDecision::Full { amount } | HurtDecision::Topup { amount } => amount,
        };
        let outcome = lodestone_entity::apply_reductions(amount, &self.defenses, flags);
        self.defenses.absorption = outcome.remaining_absorption;
        self.health = (self.health - outcome.to_health).max(0.0);
        // Every hit that is not swallowed by invulnerability opens the panic
        // window. The attacker position is carried by callers that know it;
        // environmental damage leaves the mob panicking without a retaliation
        // target.
        //
        // Placed here, in the single funnel every damage path already goes
        // through, so a new damage source cannot forget it.
        self.mob.note_hurt(None);
        // Vanilla's own armadillo hurt-handler's own tail: refresh (or start) the danger
        // memory on every hit that reached this point — see
        // `armadillo_danger_ticks`'s own doc for the disclosed narrowing
        // (real vanilla additionally requires a living-entity attacker and
        // gates the roll itself on the "can stay rolled up" check).
        if self.entity_type.path() == "armadillo" {
            self.armadillo_danger_ticks = ARMADILLO_DANGER_TICKS;
        }
        outcome.to_health
    }

    /// This mob's live status effects — see [`Self::apply_effect`] to add one.
    #[must_use]
    pub fn effects(&self) -> &crate::mob_effects::ActiveEffects {
        &self.effects
    }

    /// Applies one status effect through vanilla's own stacking rule
    /// (vanilla's own generic add-effect call → [`crate::mob_effects::EffectInstance::update`]
    /// — see that type's own doc for the "remembered, not ignored or replaced"
    /// table). Returns whether the active instance changed, matching
    /// [`crate::mob_effects::ActiveEffects::apply`]'s own return.
    pub fn apply_effect<K: crate::mob_effects::EffectKey>(
        &mut self,
        effect_id: K,
        duration: i32,
        amplifier: u32,
    ) -> bool {
        self.effects.apply(effect_id, duration, amplifier)
    }

    /// Whether this mob is visibly on fire — vanilla's own "is on fire" check's
    /// remaining-fire-ticks-positive test.
    #[must_use]
    pub fn is_on_fire(&self) -> bool {
        self.burn.is_on_fire()
    }

    /// Vanilla's own "ignite for seconds" call — raises the burn counter, never lowers it
    /// (see [`crate::burning::BurnState::ignite_for_seconds`]). The fireball
    /// impact path (`MobSim::resolve_projectile_hit`) is the only production
    /// caller today.
    pub fn ignite_for_seconds(&mut self, seconds: f32) {
        self.burn.ignite_for_seconds(seconds);
    }

    /// The mob's current position.
    #[must_use]
    pub fn position(&self) -> Vec3 {
        self.mob.position()
    }

    /// The mob's collision body — the box [`MobSim::explode`] samples for
    /// blast exposure.
    #[must_use]
    pub fn shape(&self) -> &MobShape {
        self.mob.shape()
    }

    /// How many A\* searches this mob has run — the count that proves the
    /// pathfinder is actually being driven (a stubbed `move_to` never searches).
    #[must_use]
    pub fn path_searches(&self) -> u32 {
        self.mob.path_searches()
    }

    /// Whether the mob still has a path it is following.
    #[must_use]
    pub fn has_path(&self) -> bool {
        self.mob.has_path()
    }

    /// The mob's spawn category (drives its despawn distances).
    #[must_use]
    pub fn category(&self) -> MobCategory {
        self.category
    }

    /// Sets the mob's spawn category. Used by the spawn driver so a mob's
    /// despawn behaviour matches the category it was spawned as.
    pub fn set_category(&mut self, category: MobCategory) -> &mut Self {
        self.category = category;
        self
    }

    /// The mob's current `no_action_time` age timer (ticks since it last acted).
    #[must_use]
    pub fn no_action_time(&self) -> i32 {
        self.no_action_time
    }

    /// Whether the mob is exempt from natural despawn.
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.persistent
    }

    /// Marks the mob persistent (named / persistence-required) so it never
    /// naturally despawns, mirroring vanilla `isPersistenceRequired`.
    pub fn set_persistent(&mut self, persistent: bool) -> &mut Self {
        self.persistent = persistent;
        self
    }

    /// The mob's stable UUID, encoded verbatim in the spawn packet.
    #[must_use]
    pub fn uuid(&self) -> Uuid {
        self.uuid
    }

    /// The mob's canonical entity-type key (e.g. `minecraft:zombie`). See the
    /// field docs for the placeholder caveat.
    #[must_use]
    pub fn entity_type(&self) -> &ResourceKey {
        &self.entity_type
    }

    /// Copies the six equipment slots assigned by the species spawn path.
    /// Empty slots remain explicit so observers can distinguish an empty mob
    /// slot from an unavailable snapshot, and no simulation-owned value
    /// escapes the call.
    #[must_use]
    pub fn equipment_snapshot(&self) -> Vec<EntityEquipment> {
        [
            (ModelEquipmentSlot::MainHand, self.equipment.main_hand),
            (ModelEquipmentSlot::OffHand, self.equipment.off_hand),
            (ModelEquipmentSlot::Head, self.equipment.head),
            (ModelEquipmentSlot::Chest, self.equipment.chest),
            (ModelEquipmentSlot::Legs, self.equipment.legs),
            (ModelEquipmentSlot::Feet, self.equipment.feet),
        ]
        .into_iter()
        .map(|(slot, item)| EntityEquipment {
            slot,
            item: item.map(|item| {
                ItemStack::new(
                    ResourceKey::from_str(item.name()).expect("built-in item key is valid"),
                    1,
                )
            }),
        })
        .collect()
    }

    /// Sets the mob's canonical entity-type key. Used by a species-aware spawn
    /// driver so the encoded spawn packet names the right entity.
    pub fn set_entity_type(&mut self, entity_type: ResourceKey) -> &mut Self {
        self.entity_type = entity_type;
        self
    }

    /// The mob's body rotation (degrees). Body yaw tracks the movement
    /// direction; ground mobs keep a level body, so pitch is 0.
    #[must_use]
    pub fn rotation(&self) -> Rotation {
        Rotation::new(self.mob.body_yaw(), 0.0)
    }

    /// The mob's head yaw in degrees — toward its look target if a goal set one,
    /// otherwise the body yaw. Matches `ClientEvent::EntityHeadRotation`.
    #[must_use]
    pub fn head_yaw(&self) -> f32 {
        self.mob.head_yaw()
    }

    /// The mob's velocity in **blocks per tick** (the unit vanilla's wire packing
    /// assumes), i.e. the position delta applied on the last tick.
    #[must_use]
    pub fn velocity(&self) -> Vec3 {
        self.mob.velocity()
    }

    /// This villager's profession — `villager::Profession::None` for every
    /// non-villager and every villager that has not claimed a workstation.
    #[must_use]
    pub fn profession(&self) -> villager::Profession {
        self.profession
    }

    /// The workstation this villager claimed, if any.
    #[must_use]
    pub fn workstation(&self) -> Option<BlockPos> {
        self.workstation
    }

    /// The bed this villager has claimed as its home point-of-interest, if any.
    #[must_use]
    pub fn bed(&self) -> Option<BlockPos> {
        self.bed
    }

    /// The bell this villager has claimed as its meeting point-of-interest, if any —
    /// [`bed`](Self::bed)'s sibling.
    #[must_use]
    pub fn meeting_point(&self) -> Option<BlockPos> {
        self.meeting_point
    }

    /// The nearest warden-listenable vibration this tick, if any — the
    /// vibration substrate. See the `nearest_vibration` field's own doc for
    /// what this drives ([`MobSim::resolve_warden_anger`]).
    #[must_use]
    pub fn nearest_vibration(&self) -> Option<PostedVibration> {
        self.nearest_vibration
    }

    /// This mob's own tracked anger level — see the `warden_anger` field's
    /// own doc for the single-suspect narrowing. `0` for a non-listener
    /// species.
    #[must_use]
    pub fn warden_anger(&self) -> i32 {
        self.warden_anger
    }

    /// The entity id [`warden_anger`](Self::warden_anger) is banked against,
    /// if any.
    #[must_use]
    pub fn warden_anger_target(&self) -> Option<i32> {
        self.warden_anger_target
    }

    /// Vanilla's own anger-level bucketing — this mob's own anger bucketed into vanilla's
    /// three named levels.
    #[must_use]
    pub fn warden_anger_level(&self) -> warden::AngerLevel {
        warden::AngerLevel::from_anger(self.warden_anger)
    }

    /// Vanilla's own "has left horn" check. Meaningless for a non-goat species.
    #[must_use]
    pub fn has_left_horn(&self) -> bool {
        self.has_left_horn
    }

    /// Vanilla's own "has right horn" check. Meaningless for a non-goat species.
    #[must_use]
    pub fn has_right_horn(&self) -> bool {
        self.has_right_horn
    }

    /// `minecraft:spawn_reinforcements`'s current base value. See
    /// [`reinforcement_chance`](Self::reinforcement_chance)'s own field doc.
    #[must_use]
    pub fn reinforcement_chance(&self) -> f64 {
        self.reinforcement_chance
    }

    /// `ZOMBIE_REINFORCEMENT_CALLEE_CHARGE` — the permanent `-0.05` a freshly
    /// placed reinforcement is charged against its own (independently
    /// randomized) `reinforcement_chance`, on top of whatever
    /// [`spawn_species`](Self::spawn_species)'s own
    /// vanilla-derived reinforcements-chance randomizer roll gave it, so a chain of
    /// reinforcements-calling-reinforcements tapers off rather than
    /// sustaining indefinitely. The driver (`crate::tick::run_tick_loop`)
    /// calls this on the mob [`spawn_species`](Self::spawn_species) just
    /// returned, since only it can tell "this spawn is a reinforcement" from
    /// "this spawn is anything else".
    pub fn apply_reinforcement_callee_charge(&mut self) -> &mut Self {
        self.reinforcement_chance -= ZOMBIE_REINFORCEMENT_CALLEE_CHARGE;
        self
    }

    /// Vanilla's own villager-data trade level, `1..=5`.
    #[must_use]
    pub fn villager_level(&self) -> i32 {
        self.villager_level
    }

    /// Accumulated trading xp toward the next level.
    #[must_use]
    pub fn villager_xp(&self) -> i32 {
        self.villager_xp
    }

    /// Assigns (or clears, with `villager::Profession::None`) this mob's
    /// profession and workstation together — the two always change in
    /// lockstep (see [`MobSim::tick_villager_professions`]'s doc for why a
    /// claim and a profession are never set independently).
    pub(crate) fn set_profession(
        &mut self,
        profession: villager::Profession,
        workstation: Option<BlockPos>,
    ) {
        self.profession = profession;
        self.workstation = workstation;
    }

    /// Ensures [`Self::trades`] matches the current `(profession,
    /// villager_level)`, rebuilding it from
    /// [`crate::villager_trade::VillagerTrades::for_profession`] when either
    /// has moved on since the last build — see that field's own doc for
    /// what a rebuild costs. `None` for a non-villager or an unemployed one
    /// (`Profession::None`), which get no economics at all, matching every
    /// other villager-only accessor in this file.
    fn ensure_trades(&mut self) -> Option<&mut crate::villager_trade::VillagerTrades> {
        if self.profession == villager::Profession::None {
            self.trades = None;
            return None;
        }
        let fresh = !matches!(
            &self.trades,
            Some((p, l, _)) if *p == self.profession && *l == self.villager_level
        );
        if fresh {
            let trade_level = lodestone_data::villager_trades::VillagerLevel::new(self.villager_level)
                .expect("a simulated villager always keeps a level in 1..=5");
            self.trades = Some((
                self.profession,
                self.villager_level,
                crate::villager_trade::VillagerTrades::for_profession(self.profession, trade_level),
            ));
        }
        self.trades.as_mut().map(|(_, _, trades)| trades)
    }

    /// Applies a completed trade's xp reward (`TradeRecord::xp`) and
    /// advances this villager's level via [`villager::level_up`] — vanilla's
    /// own "reward trade xp" step feeding its own "set villager xp" setter. Previously
    /// nothing in this crate ever called this: `villager_level` was
    /// initialised to `1` and never mutated again, so no villager could ever
    /// reach a level-2..5 trade no matter how much it was traded with. A
    /// level change is picked up the next [`Self::ensure_trades`] call,
    /// which rebuilds the offer list to include the newly unlocked tier.
    fn give_villager_xp(&mut self, xp: i32) {
        self.villager_xp += xp;
        self.villager_level = villager::level_up(self.villager_level, self.villager_xp);
    }

    /// Lowers the mob into a version-free [`EntitySnapshot`] for the encode seam.
    /// This is the whole identity/motion surface a [`ServerProtocol`] needs to
    /// build spawn/move/remove packets; the server holds the previous snapshot
    /// per connection so the protocol can stay stateless.
    ///
    /// `metadata` is the per-species entity-metadata field list —
    /// general across mobs (see [`MetadataField`]'s own doc comment), not a
    /// creeper-only mechanism, even though a creeper was the only producer
    /// for a long time. [`crate::server::EntityStreamer`] diffs this exactly like
    /// every other field here, so a change reaches [`ServerProtocol::encode_set_entity_data`]
    /// through the same spawn/update path `position`/`rotation` already use —
    /// no second wiring for the next mob that needs a metadata field.
    ///
    /// `CreeperSwellDir` is always included for a creeper, even at its `-1`
    /// default: unlike `CreeperIgnited` (monotonic — set once, never
    /// cleared, so *absence* safely means "still false"), `swell_dir` can
    /// legitimately return to `-1` mid-episode during retreat,
    /// and that transition must reach the client exactly like the climb to
    /// `1` did — a client that keeps whatever `swell_dir` it was last sent
    /// would integrate the fuse in the wrong direction forever if a
    /// retreat-to-`-1` were ever skipped as "just the default".
    ///
    /// `MetadataField::Baby` is the same shape as `CreeperSwellDir`, not as
    /// `CreeperIgnited`: a mob **grows up**, so absence cannot safely mean
    /// "still a baby" the way it can mean "still not ignited". It is pushed
    /// unconditionally for every species eligible for it (see the species
    /// switch below), carrying the current `is_baby()` value whether that is
    /// `true` or `false`, so the adult transition reaches the client the same
    /// way the arrival as a baby did.
    #[must_use]
    pub fn snapshot(&self) -> EntitySnapshot {
        let mut metadata = Vec::new();
        if self.entity_type.path() == "creeper" {
            metadata.push(MetadataField::CreeperSwellDir(self.swell_dir()));
            if self.is_ignited() {
                metadata.push(MetadataField::CreeperIgnited(true));
            }
        }
        // Index 18's byte, **whose layout depends on the species** — see
        // `MetadataField::TamableFlags`. The species switch has to be here, in the
        // producer, because nothing downstream can recover it: four different `BYTE`
        // fields share index 18, one apiece on the tameable-animal, horse-family,
        // sheep and shulker metadata tables, and no `entity_census` column separates them, so
        // an encoder handed a single shared "tamed" variant would have to guess.
        //
        // Emitted only for a tame mob: a wild one's byte is all-zero, which is the
        // client's own default, and `EntityStreamer::sync` skips an empty metadata
        // list entirely — so a wild mob costs no extra packet.
        //
        // Species with no arm here stream nothing, which is the honest state rather
        // than a gap to fill speculatively: a tame llama or fox needs its own flag
        // layout read off the dump first.
        if self.tame {
            match self.entity_type.path() {
                "wolf" | "cat" | "parrot" | "ocelot" => {
                    metadata.push(MetadataField::TamableFlags {
                        tame: true,
                        sitting: self.is_in_sitting_pose(),
                    });
                }
                "horse" | "donkey" | "mule" | "skeleton_horse" | "zombie_horse" => {
                    metadata.push(MetadataField::HorseFlags { tame: true });
                }
                _ => {}
            }
        }
        // Index 16's boolean, shared by the ageable-mob, zombie and zoglin
        // "is baby" metadata fields
        // (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`)
        // — but also by the creeper's own swell-direction field, an `INT`, which is why the
        // species switch has to live here rather than in a shared "is baby"
        // encoder: a `MetadataField::Baby` emitted for a creeper would write
        // a boolean where the swell direction belongs. Scoped to exactly the
        // species this sim tracks age for — [`baby_dimensions`] and
        // [`baby_speed_multiplier`]'s own species lists — which are also the
        // only species mechanically confirmed (via `.cache/mc/26.2/src/`) to
        // descend from the ageable-mob or zombie base classes, the two
        // classes whose own "is baby" field resolves to this index.
        //
        // Pushed unconditionally rather than only while `is_baby()` is true:
        // see this method's own doc comment for why the grown-up transition
        // needs the same treatment as the arrival.
        match self.entity_type.path() {
            "cow" | "mooshroom" | "sheep" | "pig" | "chicken" | "rabbit" | "wolf" | "zombie"
            | "husk" | "zombie_villager" | "drowned" | "zombified_piglin" => {
                metadata.push(MetadataField::Baby(self.is_baby()));
            }
            _ => {}
        }
        // Villager metadata field, index 19 — the field a
        // client's own villager renderer/profession-layer actually reads
        // to pick a texture. Pushed unconditionally for every villager, at
        // whatever `profession`/`villager_level` currently are (including
        // `None`/`1`), for the same reason `Baby` above is pushed
        // unconditionally: a profession transition needs to reach the client
        // the same way the initial value did, not only while it is
        // "interesting". `kind` is always `minecraft:plains` — see
        // `crate::mobs::villager`'s module doc for why biome-derived type is
        // out of scope.
        if self.entity_type.path() == "villager" {
            metadata.push(MetadataField::VillagerData {
                kind: ResourceKey::from_str("minecraft:plains").expect("static key is valid"),
                profession: ResourceKey::from_str(&format!("minecraft:{}", self.profession.path()))
                    .expect("every Profession::path() is a valid identifier path"),
                level: self.villager_level,
            });
        }
        // Vanilla's own goat "has left/right horn" metadata fields, indices
        // 19/20 — see
        // `MetadataField::GoatHorns`'s own doc for the collision this species
        // switch resolves and why it is pushed unconditionally.
        if self.entity_type.path() == "goat" {
            metadata.push(MetadataField::GoatHorns {
                has_left: self.has_left_horn,
                has_right: self.has_right_horn,
            });
        }
        // Vanilla's own axolotl "playing dead" metadata field, index 19 — same "unconditional, so
        // the reset reaches the client too" shape as `GoatHorns` above.
        if self.entity_type.path() == "axolotl" {
            metadata.push(MetadataField::PlayingDead(self.axolotl_play_dead_ticks > 0));
        }
        // Vanilla's own shared pose metadata field, index 6 — pushed unconditionally for a warden,
        // not only while emerging, for the same "the reset must reach the
        // client too" reason `Baby`/`CreeperSwellDir` are: see
        // `MetadataField::Pose`'s own doc.
        if self.entity_type.path() == "warden" {
            metadata.push(MetadataField::Pose(if self.warden_emerge_ticks > 0 {
                warden::POSE_EMERGING
            } else if self.warden_digging_ticks > 0 {
                warden::POSE_DIGGING
            } else {
                warden::POSE_STANDING
            }));
        }
        // Same "unconditional, so the reset reaches the client too" shape as
        // the warden arm above — a camel that has just stood up must send
        // the standing pose (`0`), not merely stop sending the sitting pose.
        if self.entity_type.path() == "camel" {
            metadata.push(MetadataField::Pose(if self.camel_sitting {
                CAMEL_POSE_SITTING
            } else {
                CAMEL_POSE_STANDING
            }));
            // Vanilla's own camel dash metadata field — same "unconditional, so the reset reaches the
            // client too" shape as the pose push just above: a camel that
            // has finished dashing must send `false`, not merely stop
            // sending `true`.
            metadata.push(MetadataField::Dash(self.camel_is_dashing()));
        }
        // Vanilla's own sniffer state metadata field — same "unconditional, so the reset reaches
        // the client too" shape as the camel arm above. Pushed for a
        // sniffer only; see `sniffer::SnifferState::wire_ordinal` for the
        // real jar ordinal this carries.
        if self.entity_type.path() == "sniffer" {
            metadata.push(MetadataField::SnifferState(self.sniffer_state.wire_ordinal()));
        }
        EntitySnapshot {
            id: self.id,
            uuid: self.uuid,
            entity_type: self.entity_type.clone(),
            position: self.position(),
            rotation: self.rotation(),
            head_yaw: self.head_yaw(),
            velocity: self.velocity(),
            metadata,
            // No mob supplies additional spawn data here.
            object_data: 0,
            // Resolved by `MobSim::snapshots`, not here: `leash_holder` names a
            // player by uuid, and only `MobSim` (through `self.players`) can turn
            // that into the wire entity id `EntitySnapshot::leash_link` carries.
            // `SimMob` alone has no player list to resolve against.
            leash_link: None,
        }
    }
}

/// Wire identity for one tracked projectile.
///
/// [`ProjectileRegistry`]  deliberately stays version-free — its
/// own doc comment says a caller's `id`/ballistic state is all it tracks — so
/// the uuid and canonical entity-type key a spawn packet needs live here,
/// exactly the split [`SimMob`] already makes between `NavigatingMob`'s
/// version-free body and this crate's wire metadata.
#[derive(Debug)]

