//! A raised shield blocking damage aimed at a player.
//!
//! A player raises the shield with a use-item and lowers it with a release. It
//! blocks only once it has been up for [`BLOCK_DELAY_TICKS`]. A hit is blocked
//! in full when its damage type is not in the `bypasses_shield` tag and the
//! source lies within 90 degrees of where the player faces, measured on the
//! horizontal plane. A blocked hit costs the shield durability once it is at
//! least [`ITEM_DAMAGE_THRESHOLD`], and a hit from an axe-wielding attacker also
//! disables the shield for [`AXE_DISABLE_SECONDS`].
//!
//! [`ShieldState`] is per connection. [`ShieldState::block`] is the single
//! decision point, called where a hit reaches the player's vitals.

use lodestone_data::damage_types::{DamageType, DamageTypeTag};
use lodestone_model::{ItemStack, Vec3};

/// The shield item.
pub const SHIELD_ITEM: &str = "minecraft:shield";

/// Ticks a raised shield takes to start blocking (a quarter second).
pub const BLOCK_DELAY_TICKS: u64 = 5;

/// Half-angle, in degrees, of the horizontal arc in front of the player that a
/// shield covers.
const BLOCKING_ANGLE_DEGREES: f32 = 90.0;

/// Blocked damage below this wears the shield not at all.
const ITEM_DAMAGE_THRESHOLD: f32 = 3.0;

/// Seconds a hit from an axe disables the shield for.
pub const AXE_DISABLE_SECONDS: f32 = 5.0;

/// The item cooldown group a shield's disable cooldown is registered under.
pub const COOLDOWN_GROUP: &str = SHIELD_ITEM;

/// Seconds a hit from a mob holding `main_hand` disables a shield for.
#[must_use]
pub fn disable_seconds_for_weapon(main_hand: Option<&str>) -> f32 {
    if main_hand.is_some_and(|item| item.ends_with("_axe")) {
        AXE_DISABLE_SECONDS
    } else {
        0.0
    }
}

/// A shield held up and the tick it was raised on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Raised {
    native: usize,
    since: u64,
}

/// A hit aimed at the player.
#[derive(Debug, Clone, Copy)]
pub struct IncomingHit<'a> {
    /// Damage before any reduction.
    pub damage: f32,
    /// The `minecraft:damage_type` registry name.
    pub damage_type: &'a str,
    /// Where the hit came from.
    pub source: Vec3,
    /// Seconds the attacker's weapon disables a shield for, `0` for none.
    pub disable_seconds: f32,
}

/// What blocking a hit cost and did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Blocked {
    /// Damage the shield absorbed. The whole hit, for the stock shield.
    pub damage: f32,
    /// Durability the shield loses.
    pub item_damage: u32,
    /// Ticks the shield is disabled for, `0` when the attacker cannot disable it.
    pub disable_ticks: u64,
    /// The inventory slot holding the shield.
    pub native: usize,
}

/// One connection's shield: whether it is raised, and until when it is disabled.
#[derive(Debug, Default, Clone)]
pub struct ShieldState {
    raised: Option<Raised>,
    disabled_until: u64,
}

impl ShieldState {
    /// Raises the shield in inventory slot `native` at tick `now`; refused while
    /// the shield is disabled or the slot does not hold one.
    pub fn raise(&mut self, held: Option<&ItemStack>, native: usize, now: u64) -> bool {
        self.raised = None;
        if now < self.disabled_until || !held.is_some_and(|stack| stack.item.to_string() == SHIELD_ITEM) {
            return false;
        }
        self.raised = Some(Raised { native, since: now });
        true
    }

    /// Lowers the shield.
    pub fn lower(&mut self) {
        self.raised = None;
    }

    /// Disables the shield until tick `until` and lowers it.
    pub fn disable_until(&mut self, until: u64) {
        self.disabled_until = until;
        self.raised = None;
    }

    /// Decides whether the shield blocks `hit` on a player at `position` facing
    /// `yaw` degrees. `held` reads an inventory slot; a raised slot that no
    /// longer holds a shield lowers it.
    pub fn block(
        &mut self,
        held: impl Fn(usize) -> Option<ItemStack>,
        now: u64,
        hit: &IncomingHit<'_>,
        position: Vec3,
        yaw: f32,
    ) -> Option<Blocked> {
        let IncomingHit { damage, damage_type, source, disable_seconds } = *hit;
        let raised = self.raised?;
        if !held(raised.native).is_some_and(|stack| stack.item.to_string() == SHIELD_ITEM) {
            self.raised = None;
            return None;
        }
        if now.saturating_sub(raised.since) < BLOCK_DELAY_TICKS || damage <= 0.0 {
            return None;
        }
        if DamageType::from_name(damage_type).is_some_and(|ty| ty.is_in(DamageTypeTag::BypassesShield)) {
            return None;
        }
        if horizontal_angle(source, position, yaw) > f64::from((std::f32::consts::PI / 180.0) * BLOCKING_ANGLE_DEGREES) {
            return None;
        }
        let item_damage = if damage < ITEM_DAMAGE_THRESHOLD { 0 } else { (1.0 + damage).floor() as u32 };
        let disable_ticks = if disable_seconds > 0.0 { (disable_seconds * 20.0).round() as u64 } else { 0 };
        Some(Blocked { damage, item_damage, disable_ticks, native: raised.native })
    }
}

/// The angle, in radians, between the player's facing and the horizontal
/// direction to `source`. A source directly above or below counts as 90 degrees.
fn horizontal_angle(source: Vec3, position: Vec3, yaw: f32) -> f64 {
    let yaw = f64::from(yaw).to_radians();
    let facing = (-yaw.sin(), yaw.cos());
    let (dx, dz) = (source.x - position.x, source.z - position.z);
    let length = (dx * dx + dz * dz).sqrt();
    let (tx, tz) = if length < 1.0e-5 { (0.0, 0.0) } else { (dx / length, dz / length) };
    (tx * facing.0 + tz * facing.1).clamp(-1.0, 1.0).acos()
}

/// Wears `stack` by `amount`; `true` when it broke and must be removed.
pub fn wear(stack: &mut ItemStack, amount: u32) -> bool {
    let Some(max) = crate::anvil::effective_max_damage(stack) else { return false };
    let worn = stack.components.damage.unwrap_or(0) + amount;
    stack.components.damage = Some(worn);
    worn >= max
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shield() -> ItemStack {
        ItemStack::new(SHIELD_ITEM.parse().expect("key"), 1)
    }

    fn raised_at(now: u64) -> ShieldState {
        let mut state = ShieldState::default();
        assert!(state.raise(Some(&shield()), 0, now));
        state
    }

    fn block_from(state: &mut ShieldState, now: u64, source: Vec3, ty: &str, damage: f32) -> Option<Blocked> {
        let hit = IncomingHit { damage, damage_type: ty, source, disable_seconds: 0.0 };
        state.block(|_| Some(shield()), now, &hit, Vec3::new(0.0, 0.0, 0.0), 0.0)
    }

    /// Yaw 0 faces +z, so a source at +z is dead ahead and one at -z is behind.
    #[test]
    fn a_source_ahead_is_blocked_and_one_behind_is_not() {
        let mut state = raised_at(0);
        assert!(block_from(&mut state, 5, Vec3::new(0.0, 0.0, 3.0), "mob_attack", 4.0).is_some());
        assert!(block_from(&mut state, 5, Vec3::new(0.0, 0.0, -3.0), "mob_attack", 4.0).is_none());
        // 45 degrees off to either side is inside the arc; 135 degrees is outside.
        assert!(block_from(&mut state, 5, Vec3::new(3.0, 0.0, 3.0), "mob_attack", 4.0).is_some());
        assert!(block_from(&mut state, 5, Vec3::new(3.0, 0.0, -3.0), "mob_attack", 4.0).is_none());
    }

    #[test]
    fn a_shield_blocks_only_after_the_warm_up() {
        let mut state = raised_at(100);
        let ahead = Vec3::new(0.0, 0.0, 3.0);
        assert!(block_from(&mut state, 104, ahead, "mob_attack", 4.0).is_none(), "4 ticks up");
        assert!(block_from(&mut state, 105, ahead, "mob_attack", 4.0).is_some(), "5 ticks up");
    }

    #[test]
    fn damage_types_in_the_bypass_tag_are_not_blocked() {
        let mut state = raised_at(0);
        let ahead = Vec3::new(0.0, 0.0, 3.0);
        // From the generated tag membership: magic and starvation bypass, a mob hit does not.
        assert!(block_from(&mut state, 9, ahead, "indirect_magic", 4.0).is_none());
        assert!(block_from(&mut state, 9, ahead, "starve", 4.0).is_none());
        assert!(block_from(&mut state, 9, ahead, "mob_attack", 4.0).is_some());
    }

    /// Below 3 blocked damage the shield is not worn; at or above, it loses
    /// `floor(1 + damage)`.
    #[test]
    fn wear_follows_the_threshold_then_floor_of_one_plus_damage() {
        let mut state = raised_at(0);
        let ahead = Vec3::new(0.0, 0.0, 3.0);
        assert_eq!(block_from(&mut state, 9, ahead, "mob_attack", 2.5).unwrap().item_damage, 0);
        assert_eq!(block_from(&mut state, 9, ahead, "mob_attack", 3.0).unwrap().item_damage, 4);
        assert_eq!(block_from(&mut state, 9, ahead, "mob_attack", 6.5).unwrap().item_damage, 7);
    }

    #[test]
    fn an_axe_disables_for_five_seconds_and_blocks_the_next_raise() {
        let mut state = raised_at(0);
        let incoming = IncomingHit {
            damage: 4.0,
            damage_type: "mob_attack",
            source: Vec3::new(0.0, 0.0, 3.0),
            disable_seconds: 5.0,
        };
        let hit = state.block(|_| Some(shield()), 9, &incoming, Vec3::new(0.0, 0.0, 0.0), 0.0).expect("blocked");
        assert_eq!(hit.disable_ticks, 100);
        state.disable_until(9 + hit.disable_ticks);
        assert!(!state.raise(Some(&shield()), 0, 50), "still disabled");
        assert!(state.raise(Some(&shield()), 0, 109), "cooldown over");
    }

    #[test]
    fn a_swapped_out_shield_stops_blocking() {
        let mut state = raised_at(0);
        let incoming = IncomingHit {
            damage: 4.0,
            damage_type: "mob_attack",
            source: Vec3::new(0.0, 0.0, 3.0),
            disable_seconds: 0.0,
        };
        assert!(state.block(|_| None, 9, &incoming, Vec3::new(0.0, 0.0, 0.0), 0.0).is_none());
        assert!(block_from(&mut state, 9, Vec3::new(0.0, 0.0, 3.0), "mob_attack", 4.0).is_none(), "lowered for good");
    }

    /// A shield has 336 durability: 335 worn plus one more breaks it.
    #[test]
    fn a_shield_breaks_at_its_336_durability() {
        let mut stack = shield();
        assert!(!wear(&mut stack, 335));
        assert_eq!(stack.components.damage, Some(335));
        assert!(wear(&mut stack, 1));
    }

    #[test]
    fn only_axes_disable_shields() {
        assert_eq!(disable_seconds_for_weapon(Some("minecraft:iron_axe")), 5.0);
        assert_eq!(disable_seconds_for_weapon(Some("minecraft:iron_sword")), 0.0);
        assert_eq!(disable_seconds_for_weapon(None), 0.0);
    }
}
