use super::{
    FULL_CUBE, Face, angry_villager, breaking_block_effect, bubble, crit, destroy_block_effect,
    ash, campfire_smoke, drip, explosion_emitter, firework, flame, fly_towards_position,
    happy_villager, heart, huge_explosion, lava, note, poof, smoke, splash,
    spore_blossom_air, sweep_attack, totem_of_undying, white_smoke, witch,
};
use crate::{Behaviour, DripKind, DripPhase, ParticleEngine, Sheet, SpriteSource};
use lodestone_data::block_states::StateId;
use lodestone_data::item::Item;
use lodestone_physics::{Aabb, CollisionView};

const WHITE: [f32; 3] = [1.0, 1.0, 1.0];

fn state(raw: u32) -> StateId {
    StateId::new(raw).expect("test state must be in the generated census")
}

mod blocks;
mod basic;
mod magic;
mod explosion;
mod drips;
mod particles;

