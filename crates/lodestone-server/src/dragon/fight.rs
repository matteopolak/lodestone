//! The fight controller — a port of `EnderDragonFight`
//! (`.cache/mc/26.2/src/net/minecraft/world/level/dimension/end/EnderDragonFight.java`)
//! and `DragonRespawnStage`
//! (`.../world/level/dimension/end/DragonRespawnStage.java`): the
//! "dragon already defeated" persisted flag, the scan for an existing
//! crystal/dragon/portal on world load, the boss-bar *value* (not the wire
//! packet — see the module doc below), the exit-portal block geometry, and
//! the four-crystal respawn sequence.
//!
//! # What this does not attempt
//!
//! **Obsidian pillars (`EndSpikeFeature`) are not placed by this module, or
//! anywhere in this repo.** `lodestone-worldgen`'s own End module doc says so
//! explicitly: the pillars, like the exit portal, are "structure/entity
//! work" with "a gameplay placer" rather than terrain generation, and that
//! gameplay placer has never been written. Concretely this means:
//!
//! * The crystal count and the fight's own scan do not require
//!   pillars to exist — crystals in this world are floating wherever a
//!   caller puts them, not standing on spikes 40-80 blocks up.
//! * The "caged vs. uncaged crystal" distinction issue #276 names
//!   (`EndSpikeFeature`'s `guarded` flag wraps a *short* pillar's crystal in
//!   iron bars) has no pillars to attach cages to, so it is not modelled.
//!   There is nothing to cage.
//! * [`RespawnStage::SummoningPillars`]'s pillar-summoning sub-steps are
//!   ported **faithfully as a state machine parameterized by a spike count**
//!   (see [`tick_respawn`]), so the logic is correct if pillar placement
//!   lands later — but today, called with zero spikes (the honest count in a
//!   world with none), it correctly **degenerates**: `index < spike_count`
//!   is `0 < 0`, always false, so the stage advances to `SummoningDragon` on
//!   its very first tick rather than waiting through four pillar explosions.
//!   This is vanilla's own formula evaluated at the true input, not a stub —
//!   see that function's doc for the derivation.
//!
//! **The boss-bar *wire packet* is not sent by this crate.** Issue #276
//! itself draws this line ("this crate's job is the phase/health state, not
//! the bar widget"): [`boss_bar_value`] computes exactly what
//! `ServerBossEvent.setProgress`/`setVisible` would hold, and a caller with a
//! `BOSS_EVENT` encoder (which does not exist in `ServerProtocol` today — see
//! this crate's own `protocol.rs`, off limits to this change) sends it.

use lodestone_model::BlockPos;

/// `EnderDragonFight.DRAGON_SPAWN_Y` — the fixed height respawn beams target,
/// and the height a freshly created dragon spawns at above the fight origin.
pub const DRAGON_SPAWN_Y: i32 = 128;

/// The boss-bar value this fight wants shown — `ServerBossEvent`'s two
/// fields `EnderDragonFight.tick`/`updateDragon`/`setDragonKilled` actually
/// touch (`setProgress`, `setVisible`). Color (`PINK`) and overlay
/// (`PROGRESS`) never change in vanilla, so they are not modelled as fields
/// here — a caller wiring the real `BOSS_EVENT` packet hardcodes them once,
/// same as `EnderDragonFight.init` does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BossBarValue {
    /// `dragon.getHealth() / dragon.getMaxHealth()`, clamped to `[0.0, 1.0]`
    /// the same way `ServerBossEvent.setProgress` does (vanilla's own setter
    /// clamps; a health desync that briefly reports slightly over 100% must
    /// not draw past a full bar).
    pub progress: f32,
    /// `false` once `setDragonKilled` fires (`dragonEvent.setVisible(false)`),
    /// and re-asserted `!dragonKilled` every tick by
    /// `EnderDragonFight.tick`'s first line — so a caller can compute this
    /// once per tick from [`FightState::dragon_killed`] alone rather than
    /// tracking a separate "was it just killed" edge.
    pub visible: bool,
}

/// `EnderDragonFight.updateDragon`/`tick`'s progress line, plus the
/// `!this.dragonKilled` visibility line — folded into one function since
/// both read only [`FightState::dragon_killed`] and the live health pair.
#[must_use]
pub fn boss_bar_value(dragon_killed: bool, health: f32, max_health: f32) -> BossBarValue {
    let progress = if max_health > 0.0 { (health / max_health).clamp(0.0, 1.0) } else { 0.0 };
    BossBarValue { progress, visible: !dragon_killed }
}

/// Persisted per-world fight state — the fields of `EnderDragonFight` that
/// survive a save/load round trip (`EnderDragonFight.CODEC`), minus two this
/// struct does not carry: `respawn_crystals` (the four crystal ids a live
/// respawn is tracking — [`try_respawn`]'s return value is the same
/// information) and `gateways` (now [`GatewayPool`], a real, separate type —
/// both kept out of `FightState` for the identical reason: the caller, not
/// this module, is the one that persists them, the same "data, not state
/// ownership" split this module already draws elsewhere).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FightState {
    /// `EnderDragonFight.needsStateScanning` — `true` for a fresh
    /// [`FightState::new`], and for any world saved before this flag existed
    /// (vanilla's codec default is `true`, matching "assume legacy and
    /// rescan" — the same default this constructor uses).
    pub needs_state_scanning: bool,
    /// `EnderDragonFight.dragonKilled` — **not** "has the dragon ever been
    /// killed"; see [`has_previously_killed_dragon`](Self::has_previously_killed_dragon)
    /// for that. `true` means "no dragon should currently exist in the
    /// world"; a respawn in progress clears it only at [`RespawnStage::End`].
    pub dragon_killed: bool,
    /// `EnderDragonFight.hasPreviouslyKilledDragon` — persists forever once
    /// set; gates the one-time dragon-egg placement (`setDragonKilled`) and
    /// the 12000-vs-500 XP split (`EnderDragon.tickDeath`, not ported here —
    /// see [`crate::dragon`]'s module doc).
    pub has_previously_killed_dragon: bool,
}

impl FightState {
    /// A brand-new fight, matching `EnderDragonFight.createDefault()`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            needs_state_scanning: true,
            dragon_killed: false,
            has_previously_killed_dragon: false,
        }
    }
}

impl Default for FightState {
    fn default() -> Self {
        Self::new()
    }
}

/// What [`scan_state`] found, for the caller to act on — vanilla performs
/// these actions inline inside `scanState`; this module reports them instead
/// since it has no world to act on directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanOutcome {
    /// Whether a legacy/existing dragon entity found in the world should be
    /// discarded — `EnderDragonFight.scanState`'s
    /// `"But we didn't have a portal, let's remove it."` branch: a live
    /// dragon with **no** active exit portal nearby is legacy garbage (the
    /// fight state was lost, e.g. from an old save format) and gets
    /// discarded rather than adopted.
    pub discard_existing_dragon: bool,
}

/// `EnderDragonFight.scanState` — run once (gated by
/// [`FightState::needs_state_scanning`]) the first time a world with an
/// unscanned fight loads a chunk in the arena. Mutates `state` in place and
/// returns what the caller must additionally do to the world (which this
/// module cannot do itself).
///
/// # Clauses, matching `scanState` line for line
///
/// 1. `activePortalExists = hasActiveExitPortal()` — caller-supplied
///    (`active_portal_exists`), since it requires scanning real chunk block
///    entities this module has no access to.
/// 2. If a portal exists: `hasPreviouslyKilledDragon = true` (the dragon was
///    already beaten in this world, portal already active).
/// 3. If not: `hasPreviouslyKilledDragon = false`, and if no portal pattern
///    can be found anywhere (`findExitPortal() == null`), spawn one
///    (inactive) — the caller does the actual placement via
///    [`exit_portal_blocks`] with `active = false` when this function
///    returns `needs_portal_spawn = true` in the outcome... **not modelled
///    as a return field** because this module cannot tell "no portal found
///    anywhere" from "found one already" without the caller's own scan; the
///    caller is expected to call [`exit_portal_blocks`] itself exactly when
///    its own `find_exit_portal` returned nothing, mirroring vanilla's own
///    `if (this.findExitPortal() == null) { this.spawnExitPortal(false); }`.
/// 4. `dragonKilled = entities.isEmpty()` — `existing_dragon_alive` stands in
///    for `!level.getDragons().isEmpty()`.
/// 5. If a dragon exists but there was no active portal, discard it
///    (`discard_existing_dragon` in the outcome) and forget its uuid.
/// 6. The final `if (!hasPreviouslyKilledDragon && dragonKilled) {
///    dragonKilled = false; }` correction — a world that has never seen the
///    dragon killed but also has no dragon (e.g. a freshly created End) is
///    NOT "dragon killed" (which would suppress boss-bar visibility and
///    respawn logic); it just has no dragon yet, and [`FightState::dragon_killed`]
///    staying `false` is what lets `EnderDragonFight.tick`'s
///    `findOrCreateDragon` spawn one.
pub fn scan_state(state: &mut FightState, active_portal_exists: bool, existing_dragon_alive: bool) -> ScanOutcome {
    let mut discard_existing_dragon = false;
    if active_portal_exists {
        state.has_previously_killed_dragon = true;
    } else {
        state.has_previously_killed_dragon = false;
    }

    state.dragon_killed = !existing_dragon_alive;
    if existing_dragon_alive && !active_portal_exists {
        discard_existing_dragon = true;
    }

    if !state.has_previously_killed_dragon && state.dragon_killed {
        state.dragon_killed = false;
    }

    state.needs_state_scanning = false;
    ScanOutcome { discard_existing_dragon }
}

/// What [`set_dragon_killed`] needs the caller to do — the world-side effects
/// `EnderDragonFight.setDragonKilled` performs directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeathOutcome {
    /// Place `minecraft:dragon_egg` at the podium — only on the **first**
    /// kill ever (`!this.hasPreviouslyKilledDragon` checked **before** the
    /// flag is set to `true`).
    pub place_dragon_egg: bool,
    /// Activate the exit portal (`spawnExitPortal(true)`) — always `true`
    /// here; kept as a field rather than a doc note so a caller cannot
    /// forget it is unconditional.
    pub activate_exit_portal: bool,
    /// Pop and place the next gateway (`spawnNewGateway()`) — always `true`.
    pub spawn_gateway: bool,
}

/// `EnderDragonFight.setDragonKilled` — call once, when the dragon's health
/// reaches `0.0` while [`crate::dragon::phase::Phase::Dying`]'s clean-flight
/// check ends the death sequence. Mutates `state`; the caller performs the
/// three effects in [`DeathOutcome`] against the real world.
///
/// Vanilla guards this whole method on `dragon.getUUID().equals(this.dragonUUID)`
/// — the caller's responsibility to have already checked (this module tracks
/// no dragon identity of its own).
pub fn set_dragon_killed(state: &mut FightState) -> DeathOutcome {
    let place_dragon_egg = !state.has_previously_killed_dragon;
    state.has_previously_killed_dragon = true;
    state.dragon_killed = true;
    DeathOutcome {
        place_dragon_egg,
        activate_exit_portal: true,
        spawn_gateway: true,
    }
}

/// `EndPodiumFeature.place`, ported clause for clause
/// (`.cache/mc/26.2/src/net/minecraft/world/level/levelgen/feature/EndPodiumFeature.java`).
/// `origin` is `EndPodiumFeature.getLocation` — the podium block, one below
/// the portal floor. `active` selects the killed-dragon (portal open,
/// column above cleared) vs. not-yet-killed (portal floor solid, column
/// unexcavated) variant, matching vanilla's `active` constructor flag.
///
/// Returns every `(pos, block_state)` write the feature makes, **in the same
/// order** vanilla iterates them (`BlockPos.betweenClosed` in `x, y, z`
/// nesting, then the four-block bedrock pillar, then the four torches) —
/// order matters only in that a later write in this list must win if a
/// caller applies them in sequence, exactly as vanilla's sequential
/// `setBlock` calls would.
///
/// # Clauses
///
/// 1. **Foundation ring** (`pos.getY() < origin.getY()`, i.e. exactly
///    `origin.y - 1` since the loop's `y` never goes lower): the inner disc
///    (`closerThan(origin, 2.5)`) is bedrock; the surrounding ring
///    (`2.5..3.5`) is end stone.
/// 2. **Portal ring** (`pos.getY() == origin.getY()`): the ring is bedrock;
///    the inner disc is `minecraft:end_portal` when `active`, air otherwise.
/// 3. **The shaft above** (`origin.y < pos.getY() <= origin.y + 32`, radius
///    `3.5`): always air (a killed dragon's portal chamber is open to the
///    sky the whole shaft; an unkilled one's is too — vanilla's inactive
///    branch also sets air here, `dropPreviousAndSetBlock` only changes
///    *how* air is set, not *whether*, and both write plain air blocks).
/// 4. **The central bedrock pole**, `origin.y..=origin.y+3` at exactly
///    `(origin.x, origin.z)` — unconditionally bedrock, **overwriting**
///    whatever clause 2 wrote at `y == origin.y` for that one column (the
///    portal-block disc has a hole at its exact center for the pole).
/// 5. **Four wall torches** at `origin.y + 2`, one per horizontal face.
#[must_use]
pub fn exit_portal_blocks(origin: BlockPos, active: bool) -> Vec<(BlockPos, &'static str)> {
    let mut out = Vec::new();
    for x in (origin.x - 4)..=(origin.x + 4) {
        for y in (origin.y - 1)..=(origin.y + 32) {
            for z in (origin.z - 4)..=(origin.z + 4) {
                let pos = BlockPos::new(x, y, z);
                let dx = f64::from(x - origin.x);
                let dy = f64::from(y - origin.y);
                let dz = f64::from(z - origin.z);
                let dist_sq = dx * dx + dy * dy + dz * dz;
                let inside_rim = dist_sq < 2.5 * 2.5;
                let in_ring_or_rim = inside_rim || dist_sq < 3.5 * 3.5;
                if !in_ring_or_rim {
                    continue;
                }
                if y < origin.y {
                    out.push((pos, if inside_rim { "minecraft:bedrock" } else { "minecraft:end_stone" }));
                } else if y > origin.y {
                    out.push((pos, "minecraft:air"));
                } else if !inside_rim {
                    out.push((pos, "minecraft:bedrock"));
                } else if active {
                    out.push((pos, "minecraft:end_portal"));
                } else {
                    out.push((pos, "minecraft:air"));
                }
            }
        }
    }
    for y in 0..4 {
        out.push((BlockPos::new(origin.x, origin.y + y, origin.z), "minecraft:bedrock"));
    }
    let pillar_y = origin.y + 2;
    out.push((BlockPos::new(origin.x, pillar_y, origin.z + 1), "minecraft:wall_torch[facing=south]"));
    out.push((BlockPos::new(origin.x, pillar_y, origin.z - 1), "minecraft:wall_torch[facing=north]"));
    out.push((BlockPos::new(origin.x + 1, pillar_y, origin.z), "minecraft:wall_torch[facing=east]"));
    out.push((BlockPos::new(origin.x - 1, pillar_y, origin.z), "minecraft:wall_torch[facing=west]"));
    out
}

/// `EnderDragonFight.init`'s gateway pool size — the pie is always cut into
/// twenty slices, `Range.closedOpen(0, 20)`.
pub const GATEWAY_COUNT: i32 = 20;

/// The pool of unused gateway pie-slice indices (`EnderDragonFight.gateways`)
/// — twenty of them, shuffled once and consumed one per dragon kill by
/// [`GatewayPool::pop`]. Kept out of [`FightState`] for the identical reason
/// [`try_respawn`]'s `respawn_crystals` is: the caller owns persistence, not
/// this module — see [`FightState`]'s own doc.
///
/// **Not byte-identical to vanilla's own draw order.** `EnderDragonFight
/// .init` shuffles with `RandomSource.createThreadLocalInstance(seed)`, a
/// thread-local generator whose exact algorithm is a JVM implementation
/// detail, not a reproducible formula — the same disclosed gap
/// `crate::mob_spawn`'s own module doc already states for every RNG stream
/// in this crate ("nothing here promises byte-identical RNG streams with a
/// real vanilla server"). [`shuffled`](Self::shuffled) ports `Util.shuffle`'s
/// **algorithm** (a standard Fisher–Yates walk from the end) against this
/// crate's own [`crate::mob_spawn::SpawnRng`], which yields a real, uniform,
/// non-repeating draw of all twenty slices — just not vanilla's own sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayPool(Vec<i32>);

impl GatewayPool {
    /// `EnderDragonFight.init`'s `if (this.gateways.isEmpty())` branch —
    /// `Util.shuffle` ported clause for clause (`for (i = size; i > 1; i--) {
    /// swapTo = random.nextInt(i); swap(i - 1, swapTo); }`), against
    /// `0..GATEWAY_COUNT`. See this struct's own doc for why the RNG itself
    /// is not vanilla's.
    #[must_use]
    pub fn shuffled(rng: &mut crate::mob_spawn::SpawnRng) -> Self {
        let mut slots: Vec<i32> = (0..GATEWAY_COUNT).collect();
        let mut i = slots.len();
        while i > 1 {
            let swap_to = rng.next_int(i as i32) as usize;
            slots.swap(i - 1, swap_to);
            i -= 1;
        }
        Self(slots)
    }

    /// `EnderDragonFight.spawnNewGateway`'s pop — `this.gateways.remove
    /// (this.gateways.size() - 1)`, or `None` once every slice has been used
    /// (vanilla's own `if (!this.gateways.isEmpty())` guard: a dragon killed
    /// more than twenty times spawns no further gateway).
    pub fn pop(&mut self) -> Option<i32> {
        self.0.pop()
    }

    /// Slices remaining, for a caller that wants to persist this — not done
    /// by anything in this crate today (see [`GatewayPool`]'s own doc).
    #[must_use]
    pub fn remaining(&self) -> &[i32] {
        &self.0
    }
}

/// `EnderDragonFight.spawnNewGateway`'s position formula — `gateway` is the
/// pie-slice index [`GatewayPool::pop`] returned (`0..GATEWAY_COUNT`).
/// Vanilla writes this as absolute world coordinates with **no offset by the
/// fight's own origin** (`new BlockPos(x, 75, z)`, not `origin.offset(x, 75,
/// z)`) — correct only because the one primary End dragon fight's origin is
/// always `BlockPos.ZERO`; ported the same way (no origin parameter).
#[must_use]
pub fn gateway_position(gateway: i32) -> BlockPos {
    let angle = 2.0 * (-std::f64::consts::PI + (std::f64::consts::PI / 20.0) * f64::from(gateway));
    BlockPos::new((96.0 * angle.cos()).floor() as i32, 75, (96.0 * angle.sin()).floor() as i32)
}

/// `EndGatewayFeature.place`, ported clause for clause
/// (`.cache/mc/26.2/src/net/minecraft/world/level/levelgen/feature/EndGatewayFeature.java`)
/// for the `END_GATEWAY_DELAYED` configured feature
/// (`EndGatewayConfiguration.delayedExitSearch()`: no known exit, `exact =
/// false`) — the only variant `EnderDragonFight.spawnNewGateway` ever places.
/// Writes a 3×5×3 box around `pos`: the centre column is
/// `minecraft:end_gateway` (with **no exit position set** — see this
/// function's own "What this does not attempt" note), a bedrock frame runs
/// along the four cardinal faces and caps the top/bottom of the centre
/// column, and every other cell is air.
///
/// # What this does not attempt
///
/// **The gateway's own teleport mechanic is not ported at all.** Standing in
/// a `minecraft:end_gateway` block does nothing in this crate: there is no
/// `TheEndGatewayBlockEntity`, no lazy exit-portal search (the outer-islands
/// scan `END_GATEWAY_DELAYED`'s `exact = false` triggers on first use), and
/// no teleport-on-contact entity tick. This function only places the real,
/// visible block structure the dragon's death signals — a player can see and
/// walk up to a gateway after a kill, but walking into it is inert. A real,
/// disclosed gap, the same shape [`crate::dragon::fight`]'s own module doc
/// already draws around the obsidian pillars.
#[must_use]
pub fn gateway_blocks(pos: BlockPos) -> Vec<(BlockPos, &'static str)> {
    let mut out = Vec::new();
    for x in (pos.x - 1)..=(pos.x + 1) {
        for y in (pos.y - 2)..=(pos.y + 2) {
            for z in (pos.z - 1)..=(pos.z + 1) {
                let same_x = x == pos.x;
                let same_y = y == pos.y;
                let same_z = z == pos.z;
                let end = (y - pos.y).abs() == 2;
                let cell = BlockPos::new(x, y, z);
                if same_x && same_y && same_z {
                    out.push((cell, "minecraft:end_gateway"));
                } else if same_y {
                    out.push((cell, "minecraft:air"));
                } else if end && same_x && same_z {
                    out.push((cell, "minecraft:bedrock"));
                } else if (same_x || same_z) && !end {
                    out.push((cell, "minecraft:bedrock"));
                } else {
                    out.push((cell, "minecraft:air"));
                }
            }
        }
    }
    out
}

/// `EnderDragonFight.tryRespawn`'s crystal-position check — the four cells a
/// player must have an end crystal standing in
/// (`center.relative(direction, 3)` for each horizontal `Direction`, where
/// `center = exitPortalLocation.above(1)`) before a respawn can start.
#[must_use]
pub fn respawn_crystal_positions(exit_portal_location: BlockPos) -> [BlockPos; 4] {
    let center = BlockPos::new(exit_portal_location.x, exit_portal_location.y + 1, exit_portal_location.z);
    [
        BlockPos::new(center.x, center.y, center.z - 3), // north
        BlockPos::new(center.x, center.y, center.z + 3), // south
        BlockPos::new(center.x + 3, center.y, center.z), // east
        BlockPos::new(center.x - 3, center.y, center.z), // west
    ]
}

/// `EnderDragonFight.tryRespawn` — given a lookup for "is there a live end
/// crystal occupying this cell", returns the four found crystals (in the
/// same N/S/E/W order as [`respawn_crystal_positions`]) if **all four** are
/// present, or `None` if any is missing (vanilla's early `return` on the
/// first empty list — this module checks all four rather than
/// short-circuiting, since the caller gets more information for free and
/// nothing here is expensive enough to matter).
#[must_use]
pub fn try_respawn(exit_portal_location: BlockPos, crystal_at: impl Fn(BlockPos) -> Option<i32>) -> Option<[i32; 4]> {
    let positions = respawn_crystal_positions(exit_portal_location);
    let mut found = [0i32; 4];
    for (slot, pos) in positions.iter().enumerate() {
        found[slot] = crystal_at(*pos)?;
    }
    Some(found)
}

/// `DragonRespawnStage` — the five-stage respawn spectacle. Order matches
/// the Java enum's declaration order (`START, PREPARING_TO_SUMMON_PILLARS,
/// SUMMONING_PILLARS, SUMMONING_DRAGON, END`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespawnStage {
    Start,
    PreparingToSummonPillars,
    SummoningPillars,
    SummoningDragon,
    End,
}

/// One world-side effect a [`tick_respawn`] call produced, for the caller to
/// perform (this module has no world to act on directly — same division as
/// [`ScanOutcome`]/[`DeathOutcome`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RespawnEvent {
    /// `level.levelEvent(3001, pos, 0)` — the ambient "pillar beam" sound/
    /// particle cue, at the fixed point `origin + (0, DRAGON_SPAWN_Y, 0)`.
    LevelEvent3001 { pos: BlockPos },
    /// Point every live respawn crystal's beam at `pos`.
    SetBeamTarget { pos: BlockPos },
    /// Clear every live respawn crystal's beam target (`setBeamTarget(null)`).
    ClearBeamTarget,
    /// Aim a pillar-summon beam at spike `index`'s top
    /// (`SetBeamTarget`-equivalent, but only known once the caller resolves
    /// `index` to a real spike — kept as a separate variant so a caller with
    /// no spikes yet can simply never receive one, rather than receiving a
    /// `SetBeamTarget` at a location it has to know is meaningless).
    AimAtSpike { index: usize },
    /// Destroy the 21×21×21 region around spike `index`, explode there, and
    /// re-place a real (uncaged) end spike — `SUMMONING_PILLARS`'s
    /// `endOfBeam` branch. Never emitted with `spike_count == 0`, since
    /// `index < spike_count` is then never true — see the module doc.
    SummonPillar { index: usize },
    /// The sequence finishes: reset every spike crystal's invulnerability/
    /// beam (a no-op with zero spikes), then explode and discard every
    /// respawn crystal.
    FinishAndDiscardCrystals,
}

/// `DragonRespawnStage.tick`, dispatched by current stage. `time` is
/// `EnderDragonFight.respawnTime`, reset to `0` by the caller on every stage
/// change (matching `setRespawnStage`'s unconditional `this.respawnTime =
/// 0;`) — this function does not reset it itself, since it has no mutable
/// access to the caller's counter. `spike_count` is
/// `EndSpikeFeature.getSpikesForLevel(level).size()` — always `0` in this
/// repo today (see the module doc); kept as a parameter rather than hardcoded
/// so the state machine stays correct if pillar placement lands later.
///
/// `origin` is the fight origin (`BlockPos::ZERO` for the one primary End
/// dragon fight).
#[must_use]
pub fn tick_respawn(stage: RespawnStage, time: i32, spike_count: usize, origin: BlockPos) -> (RespawnStage, Vec<RespawnEvent>) {
    let beam_target = BlockPos::new(origin.x, origin.y + DRAGON_SPAWN_Y, origin.z);
    match stage {
        RespawnStage::Start => (
            RespawnStage::PreparingToSummonPillars,
            vec![RespawnEvent::SetBeamTarget { pos: beam_target }],
        ),
        RespawnStage::PreparingToSummonPillars => {
            if time < 100 {
                let mut events = Vec::new();
                if time == 0 || time == 50 || time == 51 || time == 52 || time >= 95 {
                    events.push(RespawnEvent::LevelEvent3001 { pos: beam_target });
                }
                (RespawnStage::PreparingToSummonPillars, events)
            } else {
                (RespawnStage::SummoningPillars, Vec::new())
            }
        }
        RespawnStage::SummoningPillars => {
            let start_of_beam = time % 40 == 0;
            let end_of_beam = time % 40 == 39;
            if !start_of_beam && !end_of_beam {
                return (RespawnStage::SummoningPillars, Vec::new());
            }
            let index = (time / 40) as usize;
            if index < spike_count {
                if start_of_beam {
                    (RespawnStage::SummoningPillars, vec![RespawnEvent::AimAtSpike { index }])
                } else {
                    (RespawnStage::SummoningPillars, vec![RespawnEvent::SummonPillar { index }])
                }
            } else if start_of_beam {
                // The zero-spike degenerate case: fires on the very first
                // tick (`time == 0`, `index == 0`, `0 < 0` is false).
                (RespawnStage::SummoningDragon, Vec::new())
            } else {
                (RespawnStage::SummoningPillars, Vec::new())
            }
        }
        RespawnStage::SummoningDragon => {
            if time >= 100 {
                (RespawnStage::End, vec![RespawnEvent::FinishAndDiscardCrystals])
            } else if time >= 80 {
                (RespawnStage::SummoningDragon, vec![RespawnEvent::LevelEvent3001 { pos: beam_target }])
            } else if time == 0 {
                (RespawnStage::SummoningDragon, vec![RespawnEvent::SetBeamTarget { pos: beam_target }])
            } else if time < 5 {
                (RespawnStage::SummoningDragon, vec![RespawnEvent::LevelEvent3001 { pos: beam_target }])
            } else {
                (RespawnStage::SummoningDragon, Vec::new())
            }
        }
        RespawnStage::End => (RespawnStage::End, Vec::new()),
    }
}

/// `EnderDragonFight.onCrystalDestroyed`'s respawn-abort clause: destroying
/// one of the four active respawn crystals aborts the whole sequence
/// (`abortRespawnSequence`), rather than just reducing the alive count as it
/// would outside a respawn. `respawn_crystals` is the four ids
/// [`try_respawn`] returned when the sequence started.
#[must_use]
pub fn is_respawn_crystal(respawn_crystals: &[i32; 4], destroyed: i32) -> bool {
    respawn_crystals.contains(&destroyed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boss_bar_progress_is_health_over_max_health() {
        let v = boss_bar_value(false, 100.0, 200.0);
        assert_eq!(v.progress, 0.5);
        assert!(v.visible);
    }

    #[test]
    fn boss_bar_hidden_once_dragon_killed() {
        let v = boss_bar_value(true, 0.0, 200.0);
        assert_eq!(v.progress, 0.0);
        assert!(!v.visible);
    }

    #[test]
    fn boss_bar_progress_clamps_to_one() {
        // A transient health desync (e.g. a heal proc landing the same tick
        // as a max-health attribute change) must not draw past a full bar.
        let v = boss_bar_value(false, 250.0, 200.0);
        assert_eq!(v.progress, 1.0);
    }

    #[test]
    fn scan_state_legacy_world_with_active_portal_marks_previously_killed() {
        let mut state = FightState::new();
        let outcome = scan_state(&mut state, true, false);
        assert!(state.has_previously_killed_dragon);
        assert!(state.dragon_killed, "no dragon alive and a portal already active");
        assert!(!outcome.discard_existing_dragon);
    }

    #[test]
    fn scan_state_fresh_world_with_no_dragon_and_no_portal_is_not_dragon_killed() {
        let mut state = FightState::new();
        let outcome = scan_state(&mut state, false, false);
        // The correction clause: never-killed + no dragon alive must NOT
        // read as "dragon killed" (that would suppress spawning one).
        assert!(!state.dragon_killed);
        assert!(!state.has_previously_killed_dragon);
        assert!(!outcome.discard_existing_dragon);
    }

    #[test]
    fn scan_state_live_dragon_with_no_portal_is_legacy_garbage() {
        let mut state = FightState::new();
        let outcome = scan_state(&mut state, false, true);
        assert!(outcome.discard_existing_dragon, "a live dragon with no active portal must be discarded");
        assert!(!state.dragon_killed);
    }

    #[test]
    fn scan_state_live_dragon_with_active_portal_is_kept() {
        let mut state = FightState::new();
        let outcome = scan_state(&mut state, true, true);
        assert!(!outcome.discard_existing_dragon);
        assert!(!state.dragon_killed);
        assert!(state.has_previously_killed_dragon);
    }

    #[test]
    fn set_dragon_killed_places_egg_only_on_first_kill() {
        let mut state = FightState::new();
        let first = set_dragon_killed(&mut state);
        assert!(first.place_dragon_egg);
        assert!(state.has_previously_killed_dragon);
        assert!(state.dragon_killed);

        // Simulate a respawn + second kill: dragon_killed cleared, but
        // has_previously_killed_dragon stays true.
        state.dragon_killed = false;
        let second = set_dragon_killed(&mut state);
        assert!(!second.place_dragon_egg, "the egg is a one-time placement");
        assert!(second.activate_exit_portal);
        assert!(second.spawn_gateway);
    }

    #[test]
    fn exit_portal_geometry_matches_end_podium_feature_at_key_cells() {
        let origin = BlockPos::new(0, 64, 0);
        let active = exit_portal_blocks(origin, true);
        let find = |pos: BlockPos| active.iter().rev().find(|(p, _)| *p == pos).map(|(_, s)| *s);

        // Foundation disc directly below center: bedrock.
        assert_eq!(find(BlockPos::new(0, 63, 0)), Some("minecraft:bedrock"));
        // Foundation ring (radius ~3) below center: end stone.
        assert_eq!(find(BlockPos::new(3, 63, 0)), Some("minecraft:end_stone"));
        // Portal ring at y == origin.y, radius ~3: bedrock.
        assert_eq!(find(BlockPos::new(3, 64, 0)), Some("minecraft:bedrock"));
        // Portal disc at y == origin.y, off-center but inside the rim
        // (radius 2 < 2.5), active: end portal.
        assert_eq!(find(BlockPos::new(2, 64, 0)), Some("minecraft:end_portal"));
        // The dome above, off-center and within the 3.5 radius: air. The
        // clause is `pos.closerThan(origin, 2.5 or 3.5)` in **3D**, so this
        // is a dome, not an open vertical shaft — a point straight up from
        // center at y=70 (dy=6) is outside even the 3.5 radius and is never
        // written at all (checked below).
        assert_eq!(find(BlockPos::new(2, 65, 0)), Some("minecraft:air"));
        // The central bedrock pole overwrites the portal disc at the exact
        // center column, y == origin.y.
        assert_eq!(find(BlockPos::new(0, 64, 0)), Some("minecraft:bedrock"));
        assert_eq!(find(BlockPos::new(0, 66, 0)), Some("minecraft:bedrock"));
        // Outside every radius (dy alone exceeds 3.5): never written at all,
        // proving the dome is bounded rather than an open shaft to the sky.
        assert_eq!(find(BlockPos::new(0, 68, 0)), None);
        // Torches at the pole's second block, one per horizontal face.
        assert!(active.contains(&(BlockPos::new(0, 66, 1), "minecraft:wall_torch[facing=south]")));
        assert!(active.contains(&(BlockPos::new(0, 66, -1), "minecraft:wall_torch[facing=north]")));
        assert!(active.contains(&(BlockPos::new(1, 66, 0), "minecraft:wall_torch[facing=east]")));
        assert!(active.contains(&(BlockPos::new(-1, 66, 0), "minecraft:wall_torch[facing=west]")));
    }

    #[test]
    fn exit_portal_inactive_has_air_not_end_portal_at_the_disc() {
        let origin = BlockPos::new(0, 64, 0);
        let inactive = exit_portal_blocks(origin, false);
        let find = |pos: BlockPos| inactive.iter().rev().find(|(p, _)| *p == pos).map(|(_, s)| *s);
        assert_eq!(find(BlockPos::new(2, 64, 0)), Some("minecraft:air"));
        // The pole and ring are identical either way.
        assert_eq!(find(BlockPos::new(0, 64, 0)), Some("minecraft:bedrock"));
        assert_eq!(find(BlockPos::new(3, 64, 0)), Some("minecraft:bedrock"));
    }

    #[test]
    fn respawn_crystal_positions_are_three_out_each_cardinal_direction() {
        let portal = BlockPos::new(0, 64, 0);
        let positions = respawn_crystal_positions(portal);
        assert_eq!(positions, [
            BlockPos::new(0, 65, -3),
            BlockPos::new(0, 65, 3),
            BlockPos::new(3, 65, 0),
            BlockPos::new(-3, 65, 0),
        ]);
    }

    #[test]
    fn try_respawn_needs_all_four_crystals() {
        let portal = BlockPos::new(0, 64, 0);
        let positions = respawn_crystal_positions(portal);
        // Only three of four present.
        let present: std::collections::HashMap<BlockPos, i32> =
            positions[..3].iter().enumerate().map(|(i, p)| (*p, i as i32)).collect();
        assert_eq!(try_respawn(portal, |p| present.get(&p).copied()), None);

        let present: std::collections::HashMap<BlockPos, i32> =
            positions.iter().enumerate().map(|(i, p)| (*p, i as i32)).collect();
        assert_eq!(try_respawn(portal, |p| present.get(&p).copied()), Some([0, 1, 2, 3]));
    }

    #[test]
    fn respawn_stage_start_sets_beam_and_advances() {
        let (next, events) = tick_respawn(RespawnStage::Start, 0, 0, BlockPos::new(0, 0, 0));
        assert_eq!(next, RespawnStage::PreparingToSummonPillars);
        assert_eq!(events, vec![RespawnEvent::SetBeamTarget { pos: BlockPos::new(0, 128, 0) }]);
    }

    #[test]
    fn respawn_stage_preparing_advances_at_exactly_100_ticks() {
        let (next, _) = tick_respawn(RespawnStage::PreparingToSummonPillars, 99, 0, BlockPos::new(0, 0, 0));
        assert_eq!(next, RespawnStage::PreparingToSummonPillars, "tick 99 is still below the 100-tick threshold");
        let (next, _) = tick_respawn(RespawnStage::PreparingToSummonPillars, 100, 0, BlockPos::new(0, 0, 0));
        assert_eq!(next, RespawnStage::SummoningPillars);
    }

    #[test]
    fn respawn_stage_summoning_pillars_degenerates_instantly_with_zero_spikes() {
        // The load-bearing zero-spike case: this repo has no obsidian
        // pillars anywhere, so spike_count is always 0 in production, and
        // the formula must still behave correctly rather than getting stuck.
        let (next, events) = tick_respawn(RespawnStage::SummoningPillars, 0, 0, BlockPos::new(0, 0, 0));
        assert_eq!(next, RespawnStage::SummoningDragon);
        assert!(events.is_empty());
    }

    #[test]
    fn respawn_stage_summoning_pillars_with_real_spikes_waits_for_each() {
        let (next, events) = tick_respawn(RespawnStage::SummoningPillars, 0, 2, BlockPos::new(0, 0, 0));
        assert_eq!(next, RespawnStage::SummoningPillars);
        assert_eq!(events, vec![RespawnEvent::AimAtSpike { index: 0 }]);

        let (next, events) = tick_respawn(RespawnStage::SummoningPillars, 39, 2, BlockPos::new(0, 0, 0));
        assert_eq!(next, RespawnStage::SummoningPillars);
        assert_eq!(events, vec![RespawnEvent::SummonPillar { index: 0 }]);

        // After both spikes (index 0 and 1) are done, index 2 with
        // spike_count 2 means `2 < 2` is false -> advance.
        let (next, _) = tick_respawn(RespawnStage::SummoningPillars, 80, 2, BlockPos::new(0, 0, 0));
        assert_eq!(next, RespawnStage::SummoningDragon);
    }

    #[test]
    fn respawn_stage_summoning_dragon_finishes_at_100_ticks() {
        let (next, events) = tick_respawn(RespawnStage::SummoningDragon, 100, 0, BlockPos::new(0, 0, 0));
        assert_eq!(next, RespawnStage::End);
        assert_eq!(events, vec![RespawnEvent::FinishAndDiscardCrystals]);
    }

    #[test]
    fn respawn_stage_end_is_a_fixed_point() {
        let (next, events) = tick_respawn(RespawnStage::End, 0, 0, BlockPos::new(0, 0, 0));
        assert_eq!(next, RespawnStage::End);
        assert!(events.is_empty());
    }

    #[test]
    fn is_respawn_crystal_checks_membership() {
        let crystals = [1, 2, 3, 4];
        assert!(is_respawn_crystal(&crystals, 3));
        assert!(!is_respawn_crystal(&crystals, 5));
    }

    /// Ground truth from an independent Python `math.floor(96 *
    /// math.cos/sin(2*(-pi + (pi/20)*g)))` evaluation, not from this
    /// function — three non-round slices, none at a cardinal angle that
    /// would coincide with a wrong sign convention.
    #[test]
    fn gateway_position_matches_hand_computed_values() {
        assert_eq!(gateway_position(0), BlockPos::new(96, 75, 0));
        assert_eq!(gateway_position(5), BlockPos::new(-1, 75, 96));
        assert_eq!(gateway_position(19), BlockPos::new(91, 75, -30));
    }

    #[test]
    fn gateway_pool_shuffles_a_real_permutation_of_all_twenty_slices() {
        let mut rng = crate::mob_spawn::SpawnRng::new(1234);
        let mut pool = GatewayPool::shuffled(&mut rng);
        let mut popped = Vec::new();
        while let Some(g) = pool.pop() {
            popped.push(g);
        }
        popped.sort_unstable();
        assert_eq!(
            popped,
            (0..GATEWAY_COUNT).collect::<Vec<_>>(),
            "every one of the twenty slices must appear exactly once"
        );
        assert_eq!(pool.pop(), None, "a 21st pop must find nothing left");
    }

    /// The discriminating control against "shuffled just hands back `0..20`
    /// in order (or reverse order)": with a real RNG stream over the
    /// [`SpawnRng`](crate::mob_spawn::SpawnRng) seed below, both coincide
    /// with vanishing probability, and this seed is pinned so the assertion
    /// is exact rather than statistical.
    #[test]
    fn gateway_pool_order_is_not_the_identity_or_its_reverse() {
        let mut rng = crate::mob_spawn::SpawnRng::new(1234);
        let mut pool = GatewayPool::shuffled(&mut rng);
        let mut order = Vec::new();
        while let Some(g) = pool.pop() {
            order.push(g);
        }
        let identity: Vec<i32> = (0..GATEWAY_COUNT).collect();
        let reversed: Vec<i32> = (0..GATEWAY_COUNT).rev().collect();
        assert_ne!(order, identity);
        assert_ne!(order, reversed);
    }

    #[test]
    fn gateway_blocks_places_the_real_3x5x3_structure() {
        let pos = BlockPos::new(10, 75, -20);
        let blocks = gateway_blocks(pos);
        assert_eq!(blocks.len(), 45, "a 3x5x3 box, one write per cell");
        let at = |p: BlockPos| blocks.iter().find(|(bp, _)| *bp == p).map(|(_, s)| *s);

        assert_eq!(at(pos), Some("minecraft:end_gateway"), "the centre cell carries the gateway itself");
        assert_eq!(
            at(BlockPos::new(pos.x, pos.y - 1, pos.z)),
            Some("minecraft:bedrock"),
            "the frame column, one below the gateway"
        );
        assert_eq!(
            at(BlockPos::new(pos.x, pos.y + 2, pos.z)),
            Some("minecraft:bedrock"),
            "the bedrock cap, two above the gateway"
        );
        assert_eq!(
            at(BlockPos::new(pos.x + 1, pos.y + 2, pos.z + 1)),
            Some("minecraft:air"),
            "a true corner of the box carries neither the frame nor the gateway"
        );
    }
}
