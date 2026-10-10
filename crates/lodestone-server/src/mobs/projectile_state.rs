//! Arrow-family and thrown-item state: stuck-in-block behaviour, the pickup
//! rule, and the save/restore of both.
//!
//! An arrow, spectral arrow or trident that strikes a block does not vanish: it
//! freezes where it landed, rattles for [`SHAKE_TICKS`], and from then on a
//! player standing in reach may take it back according to [`ArrowPickup`]. A
//! stuck arrow despawns after [`ARROW_LIFETIME_TICKS`] (a trident a player may
//! reclaim never does). Every other thrown projectile is destroyed by a block,
//! as before.
//!
//! Saved fields use the names a vanilla server writes, so a world carrying an
//! arrow made by either side loads on the other: `Owner`, `LeftOwner`,
//! `HasBeenShot` on every projectile; `life`, `inBlockState`, `shake`,
//! `inGround`, `pickup`, `damage`, `crit`, `PierceLevel`, `item` on the arrow
//! family, `DealtDamage` on a trident; `Item` (a stack with components) on a
//! thrown item.

use lodestone_core::Nbt;
use lodestone_data::block_states::StateId;
use lodestone_data::potion::{PotionId, potion_name};

use super::*;
use crate::entity_record::{SavedEntity, field, read_uuid, uuid_to_ints};

/// Ticks a stuck arrow lives before it despawns.
pub const ARROW_LIFETIME_TICKS: i16 = 1200;

/// Ticks an arrow rattles after landing, during which it cannot be picked up.
pub const SHAKE_TICKS: i8 = 7;

/// Who may take an arrow back out of the world. The saved byte is the
/// discriminant: `0` disallowed, `1` allowed, `2` creative-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrowPickup {
    /// Mob-fired arrows: nobody picks them up.
    Disallowed,
    /// A survival player's own shot: taking it returns the arrow item.
    Allowed,
    /// A creative player's shot: only a creative player can clear it, and it
    /// gives nothing back.
    CreativeOnly,
}

impl ArrowPickup {
    /// The saved `pickup` byte.
    #[must_use]
    pub const fn to_byte(self) -> i8 {
        match self {
            Self::Disallowed => 0,
            Self::Allowed => 1,
            Self::CreativeOnly => 2,
        }
    }

    /// Reads a saved `pickup` byte; anything unknown is [`Self::Disallowed`].
    #[must_use]
    pub const fn from_byte(byte: i8) -> Self {
        match byte {
            1 => Self::Allowed,
            2 => Self::CreativeOnly,
            _ => Self::Disallowed,
        }
    }

    /// The rule a player's own shot starts with.
    #[must_use]
    pub const fn for_shooter(creative: bool) -> Self {
        if creative { Self::CreativeOnly } else { Self::Allowed }
    }
}

/// The arrow-family state beyond position and velocity.
#[derive(Debug, Clone)]
pub(super) struct ArrowState {
    /// The block this arrow is embedded in, `None` while it flies.
    pub(super) in_block: Option<StateId>,
    /// Ticks spent stuck, toward [`ARROW_LIFETIME_TICKS`].
    pub(super) life: i16,
    /// Remaining rattle ticks; pickup is refused while positive.
    pub(super) shake: i8,
    pub(super) pickup: ArrowPickup,
    pub(super) damage: f64,
    pub(super) crit: bool,
    pub(super) pierce: u8,
    /// The item taking this arrow back yields.
    pub(super) item: ResourceKey,
    /// A trident that has already struck an entity.
    pub(super) dealt_damage: bool,
    /// The flight direction `(yaw, pitch)` captured when it stopped, since a
    /// frozen projectile has no velocity to derive it from.
    pub(super) facing: (f32, f32),
}

impl ArrowState {
    /// A fresh state for an arrow-family entity path, or `None` for any other
    /// projectile.
    pub(super) fn new(path: &str) -> Option<Self> {
        let damage = match path {
            "arrow" | "spectral_arrow" => lodestone_entity::projectile::ARROW_BASE_DAMAGE,
            "trident" => lodestone_entity::projectile::TRIDENT_BASE_DAMAGE,
            _ => return None,
        };
        Some(Self {
            in_block: None,
            life: 0,
            shake: 0,
            pickup: ArrowPickup::Disallowed,
            damage,
            crit: false,
            pierce: 0,
            item: ResourceKey::new("minecraft", path).ok()?,
            dealt_damage: false,
            facing: (0.0, 0.0),
        })
    }

    /// What taking this arrow does for a player: `None` when it cannot be
    /// taken, `Some(None)` when it is taken but returns nothing, and
    /// `Some(Some(item))` when it returns `item`.
    ///
    /// Embedded and finished rattling is required in every case.
    pub(super) fn pickup_grant(&self, creative: bool) -> Option<Option<ResourceKey>> {
        if self.in_block.is_none() || self.shake > 0 {
            return None;
        }
        match self.pickup {
            ArrowPickup::Disallowed => None,
            ArrowPickup::Allowed => Some(Some(self.item.clone())),
            ArrowPickup::CreativeOnly => creative.then_some(None),
        }
    }
}

/// The `(yaw, pitch)` in degrees of a flight direction, the angles an arrow
/// is drawn at.
fn facing_of(velocity: Vec3) -> (f32, f32) {
    let horizontal = velocity.x.hypot(velocity.z);
    (
        velocity.x.atan2(velocity.z).to_degrees() as f32,
        velocity.y.atan2(horizontal).to_degrees() as f32,
    )
}

/// The item a thrown entity path is made from, which is the stack the save
/// carries and the one a pickup would return.
fn throwable_item(path: &str) -> &str {
    path
}

fn stack_nbt(item: &ResourceKey, potion: Option<PotionId>) -> Nbt {
    let mut fields = vec![
        ("id".to_owned(), Nbt::String(item.to_string())),
        ("count".to_owned(), Nbt::Int(1)),
    ];
    if let Some(potion) = potion {
        fields.push((
            "components".to_owned(),
            Nbt::Compound(vec![(
                "minecraft:potion_contents".to_owned(),
                Nbt::String(potion_name(potion).to_owned()),
            )]),
        ));
    }
    Nbt::Compound(fields)
}

/// The potion a saved stack's `components` name, accepting both the bare
/// string and the object form `{potion: ...}`.
fn potion_of_stack(stack: &Nbt) -> Option<PotionId> {
    let contents = field(field(stack, "components")?, "minecraft:potion_contents")?;
    match contents {
        Nbt::String(name) => PotionId::from_name(name),
        Nbt::Compound(_) => match field(contents, "potion")? {
            Nbt::String(name) => PotionId::from_name(name),
            _ => None,
        },
        _ => None,
    }
}

fn byte(flag: bool) -> Nbt {
    Nbt::Byte(i8::from(flag))
}

impl<'w> MobSim<'w> {
    /// Records the player who fired a projectile and, for the arrow family,
    /// who may take it back.
    pub fn set_projectile_shooter(&mut self, id: i32, shooter: Uuid, pickup: ArrowPickup) {
        if let Some(meta) = self.projectile_meta.get_mut(&id) {
            meta.owner_uuid = Some(shooter);
            if let Some(arrow) = &mut meta.arrow {
                arrow.pickup = pickup;
            }
        }
    }

    /// Freezes an arrow-family projectile into the block at `cell`, which it
    /// struck at `point`.
    pub(crate) fn stick_projectile(&mut self, id: i32, point: Vec3, cell: BlockPos) {
        let Some(mut tracked) = self.projectiles.iter().copied().find(|t| t.id == id) else {
            return;
        };
        let facing = facing_of(tracked.projectile.velocity);
        tracked.projectile.position = point;
        tracked.projectile.velocity = Vec3::new(0.0, 0.0, 0.0);
        tracked.projectile.frozen = true;
        self.projectiles.replace(tracked);
        let state = self.world.block_state_id(cell.x, cell.y, cell.z);
        if let Some(meta) = self.projectile_meta.get_mut(&id) {
            meta.tick_owner = projectiles::ProjectileTickOwner::for_position(point);
            if let Some(arrow) = &mut meta.arrow {
                arrow.in_block = Some(state);
                arrow.shake = SHAKE_TICKS;
                arrow.facing = facing;
            }
        }
    }

    /// Advances every stuck arrow one tick: the rattle counts down, the
    /// lifetime counts up, an arrow past its lifetime despawns, and one whose
    /// block was removed starts falling again.
    pub(crate) fn tick_stuck_arrows(&mut self) {
        let mut expired = Vec::new();
        let mut stuck = Vec::new();
        for (&id, meta) in &mut self.projectile_meta {
            let trident = meta.entity_type.path() == "trident";
            let Some(arrow) = meta.arrow.as_mut() else { continue };
            if arrow.in_block.is_none() {
                continue;
            }
            arrow.shake = (arrow.shake - 1).max(0);
            arrow.life = arrow.life.saturating_add(1);
            if arrow.life >= ARROW_LIFETIME_TICKS
                && !(trident && arrow.pickup == ArrowPickup::Allowed)
            {
                expired.push(id);
            } else {
                stuck.push(id);
            }
        }
        for id in expired {
            self.remove_projectile(id);
        }
        for id in stuck {
            let Some(mut tracked) = self.projectiles.iter().copied().find(|t| t.id == id) else {
                continue;
            };
            let at = tracked.projectile.position;
            let cell = (at.x.floor() as i32, at.y.floor() as i32, at.z.floor() as i32);
            let loaded = self
                .world
                .column(cell.0.div_euclid(16), cell.2.div_euclid(16))
                .is_some();
            if loaded && !self.world.is_solid(cell.0, cell.1, cell.2) {
                tracked.projectile.frozen = false;
                self.projectiles.replace(tracked);
                if let Some(arrow) = self
                    .projectile_meta
                    .get_mut(&id)
                    .and_then(|meta| meta.arrow.as_mut())
                {
                    arrow.in_block = None;
                }
            }
        }
    }

    /// Every stuck arrow `player_feet` can reach that the pickup rule lets
    /// this player take, as `(entity id, item to give)`. Nothing is removed:
    /// the caller banks the item first and then calls
    /// [`remove_projectile`](Self::remove_projectile), so a full inventory
    /// leaves the arrow in the world.
    #[must_use]
    pub fn arrows_within_pickup_range(
        &self,
        player_feet: Vec3,
        creative: bool,
    ) -> Vec<(i32, Option<ResourceKey>)> {
        let mut out: Vec<(i32, Option<ResourceKey>)> = self
            .projectiles
            .iter()
            .filter(|t| {
                crate::block_drops::is_within_pickup_range(player_feet, t.projectile.position)
            })
            .filter_map(|t| {
                let arrow = self.projectile_meta.get(&t.id)?.arrow.as_ref()?;
                Some((t.id, arrow.pickup_grant(creative)?))
            })
            .collect();
        out.sort_by_key(|&(id, _)| id);
        out
    }

    /// The disk records of every arrow, trident and thrown item in flight or
    /// stuck in a block.
    pub(super) fn saved_projectiles(&self) -> Vec<SavedEntity> {
        let mut out = Vec::new();
        for tracked in self.projectiles.iter() {
            let Some(meta) = self.projectile_meta.get(&tracked.id) else {
                continue;
            };
            let path = meta.entity_type.path();
            let mut extra: Vec<(String, Nbt)> = Vec::new();
            let owner = meta.owner_uuid.or_else(|| {
                let id = meta.owner?;
                self.mobs.iter().find(|m| m.id == id).map(|m| m.uuid)
            });
            if let Some(owner) = owner {
                extra.push(("Owner".to_owned(), Nbt::IntArray(uuid_to_ints(owner))));
            }
            extra.push(("HasBeenShot".to_owned(), byte(true)));
            extra.push(("LeftOwner".to_owned(), byte(meta.left_owner)));
            let facing = if let Some(arrow) = &meta.arrow {
                extra.push(("life".to_owned(), Nbt::Short(arrow.life)));
                extra.push(("shake".to_owned(), Nbt::Byte(arrow.shake)));
                extra.push(("inGround".to_owned(), byte(arrow.in_block.is_some())));
                if let Some(state) = arrow.in_block {
                    extra.push((
                        "inBlockState".to_owned(),
                        crate::chunk_nbt::state_to_palette_entry(state),
                    ));
                }
                extra.push(("pickup".to_owned(), Nbt::Byte(arrow.pickup.to_byte())));
                extra.push(("damage".to_owned(), Nbt::Double(arrow.damage)));
                extra.push(("crit".to_owned(), byte(arrow.crit)));
                extra.push(("PierceLevel".to_owned(), Nbt::Byte(arrow.pierce as i8)));
                extra.push(("item".to_owned(), stack_nbt(&arrow.item, None)));
                if path == "trident" {
                    extra.push(("DealtDamage".to_owned(), byte(arrow.dealt_damage)));
                }
                if arrow.in_block.is_some() {
                    arrow.facing
                } else {
                    facing_of(tracked.projectile.velocity)
                }
            } else {
                if let Ok(item) = ResourceKey::new("minecraft", throwable_item(path)) {
                    extra.push(("Item".to_owned(), stack_nbt(&item, meta.potion)));
                }
                facing_of(tracked.projectile.velocity)
            };
            out.push(SavedEntity {
                id: meta.entity_type.clone(),
                uuid: meta.uuid,
                pos: tracked.projectile.position,
                motion: tracked.projectile.velocity,
                rotation: Rotation::new(facing.0, facing.1),
                health: None,
                item: None,
                age: None,
                pickup_delay: None,
                extra,
            });
        }
        out
    }

    /// Re-creates one saved projectile. Returns `false` when the record is
    /// not a projectile this sim models.
    pub(super) fn restore_projectile(&mut self, saved: &SavedEntity) -> bool {
        use lodestone_entity::projectile::Projectile;
        let path = saved.id.path();
        let arrow_family = matches!(path, "arrow" | "spectral_arrow" | "trident");
        let throwable = matches!(
            path,
            "snowball"
                | "egg"
                | "ender_pearl"
                | "splash_potion"
                | "lingering_potion"
                | "experience_bottle"
        );
        if !arrow_family && !throwable {
            return false;
        }
        let get = |name: &str| {
            saved
                .extra
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value)
        };
        let flag = |name: &str| matches!(get(name), Some(Nbt::Byte(b)) if *b != 0);
        let mut projectile = if arrow_family {
            Projectile::arrow(saved.pos, saved.motion)
        } else {
            Projectile::throwable(saved.pos, saved.motion)
        };
        let potion = get("Item").and_then(potion_of_stack);
        let owner_uuid = read_uuid(get("Owner"));
        let mut arrow = ArrowState::new(path);
        if let Some(arrow) = &mut arrow {
            if let Some(Nbt::Short(life)) = get("life") {
                arrow.life = *life;
            }
            if let Some(Nbt::Byte(shake)) = get("shake") {
                arrow.shake = *shake;
            }
            if let Some(Nbt::Byte(pickup)) = get("pickup") {
                arrow.pickup = ArrowPickup::from_byte(*pickup);
            }
            if let Some(Nbt::Double(damage)) = get("damage") {
                arrow.damage = *damage;
            }
            arrow.crit = flag("crit");
            if let Some(Nbt::Byte(pierce)) = get("PierceLevel") {
                arrow.pierce = u8::try_from(*pierce).unwrap_or(0);
            }
            if let Some(stack) = get("item")
                && let Some(Nbt::String(id)) = field(stack, "id")
                && let Ok(key) = id.parse::<ResourceKey>()
            {
                arrow.item = key;
            }
            arrow.dealt_damage = flag("DealtDamage");
            arrow.facing = (saved.rotation.yaw, saved.rotation.pitch);
            if flag("inGround") {
                let state = get("inBlockState")
                    .and_then(|entry| crate::chunk_nbt::palette_entry_to_state(entry, "inBlockState").ok());
                if let Some(state) = state {
                    arrow.in_block = Some(state);
                    projectile.frozen = true;
                    projectile.velocity = Vec3::new(0.0, 0.0, 0.0);
                }
            }
        }
        let owner = owner_uuid
            .and_then(|uuid| self.mobs.iter().find(|m| m.uuid == uuid).map(|m| m.id));
        let id = self.spawn_projectile_from(saved.id.clone(), projectile, owner);
        if let Some(meta) = self.projectile_meta.get_mut(&id) {
            meta.uuid = saved.uuid;
            meta.owner_uuid = owner_uuid;
            meta.left_owner = flag("LeftOwner");
            meta.potion = potion;
            meta.arrow = arrow;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stuck(pickup: ArrowPickup, shake: i8) -> ArrowState {
        let mut arrow = ArrowState::new("arrow").expect("arrow state");
        arrow.in_block = Some(lodestone_data::block_states::air_state());
        arrow.pickup = pickup;
        arrow.shake = shake;
        arrow
    }

    /// The vanilla pickup table: who gets what, in survival and creative,
    /// and that a rattling or loose arrow cannot be taken at all.
    #[test]
    fn pickup_rule_matches_the_vanilla_table() {
        let arrow: ResourceKey = "minecraft:arrow".parse().expect("key");
        for creative in [false, true] {
            assert_eq!(stuck(ArrowPickup::Disallowed, 0).pickup_grant(creative), None);
            assert_eq!(
                stuck(ArrowPickup::Allowed, 0).pickup_grant(creative),
                Some(Some(arrow.clone())),
                "an allowed arrow returns the item to either game mode"
            );
        }
        assert_eq!(
            stuck(ArrowPickup::CreativeOnly, 0).pickup_grant(false),
            None,
            "a creative-only arrow is refused to a survival player"
        );
        assert_eq!(
            stuck(ArrowPickup::CreativeOnly, 0).pickup_grant(true),
            Some(None),
            "a creative player clears it and receives nothing"
        );
        // Control: the same arrow that was just takeable is refused while it
        // rattles, and while it is still in flight.
        assert_eq!(stuck(ArrowPickup::Allowed, 3).pickup_grant(false), None);
        let mut flying = stuck(ArrowPickup::Allowed, 0);
        flying.in_block = None;
        assert_eq!(flying.pickup_grant(false), None);
    }

    #[test]
    fn the_saved_pickup_byte_round_trips_and_unknown_is_disallowed() {
        for pickup in [ArrowPickup::Disallowed, ArrowPickup::Allowed, ArrowPickup::CreativeOnly] {
            assert_eq!(ArrowPickup::from_byte(pickup.to_byte()), pickup);
        }
        assert_eq!(ArrowPickup::from_byte(9), ArrowPickup::Disallowed);
        assert_eq!(ArrowPickup::for_shooter(true), ArrowPickup::CreativeOnly);
        assert_eq!(ArrowPickup::for_shooter(false), ArrowPickup::Allowed);
    }
}
