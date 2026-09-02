//! Legacy `world_particles` numeric particle-id → modern `minecraft:particle_type`
//! registry key, for protocol 340 (Minecraft 1.12.2).
//!
//! Unlike [`crate::sound_ids`] and [`crate::entity_types`], this is **not** a
//! namespace-prefix or a stable-id table: 1.13's flattening replaced the whole
//! `EnumParticleTypes` enum with a data-driven registry, and about a dozen
//! entries were genuinely *renamed* rather than reindexed, so there is no
//! single upstream JSON file this table can be mechanically regenerated from
//! (contrast `sound_ids.rs`, which is a bare `format!("minecraft:{name}")`
//! over `sounds.json`). `vendor/minecraft-data` ships the two sides
//! separately — `data/pc/1.12/particles.json` (legacy id → legacy name) and
//! `data/pc/1.13/particles.json` (modern registration order → modern name,
//! alphabetical, so *position* carries no correspondence) — but no crosswalk
//! between them.
//!
//! # How this table was derived
//!
//! `container` (see `docs/oracle-runtimes.md`) ran `eclipse-temurin:25-jdk`
//! against the cached `.cache/mc/1.13.2/server.jar` (no legacy-name string
//! anywhere in that jar confirms Mojang shipped no rename fixer to reverse-
//! engineer). `vineflower` decompiled the obfuscated particle-registry class
//! (registration order matches `data/pc/1.13/particles.json` exactly) and,
//! separately, every one of its ~2,800 root-package game classes was
//! disassembled with `javap -c -p -constants` and grepped for
//! `Field <registry-class>.<field>` to find every real server-side call site
//! of each modern particle constant. The explosion trio was the one case with
//! three similarly-named modern candidates and no forced 1:1 elimination:
//! `Explosion`'s own decompiled method settled it directly — `this.i >= 2.0F
//! && this.b` (size ≥ 2 and smoking) selects `explosion_emitter`, else
//! `explosion`; the per-block debris burst is unconditionally `poof`; the
//! secondary smoke particle is `smoke` — which is exactly vanilla 1.12's own
//! `EXPLOSION_HUGE`/`EXPLOSION_LARGE`/`EXPLOSION_NORMAL`/`SMOKE_NORMAL` split,
//! confirming the identity mappings alongside it. `iconcrack`/`blockcrack`/
//! `blockdust` → `item`/`block`/`block` came from the same search: `block`
//! (the block-state-parameterized particle) is referenced from four game
//! classes including the base entity-movement class (matching legacy
//! `blockdust`'s "kicked up while walking" behaviour) and a dispenser/inventory
//! class (matching legacy `blockcrack`'s "block breaking" debris) — i.e. 1.13
//! merged the two into one particle type, so both legacy ids resolve to the
//! same modern key. `item` is referenced from a class matching legacy
//! `iconcrack`'s "item stack" debris. Every other renamed entry (the five
//! `spell`/effect-family ids, `magicCrit`, `enchantmenttable`, `mobappearance`,
//! `snowballpoof`/`slime`, `reddust`) is a **forced 1:1 correspondence**: once
//! the identity and behaviourally-confirmed entries are removed from both the
//! 49-entry legacy list and the 50-entry modern list, the remaining legacy
//! names and remaining modern names pair off by direct semantic identity with
//! no alternative candidate left on either side — not a guess, a deduction
//! from the two real, ground-truth lists.
//!
//! # The seven ids with no modern key
//!
//! `suspended`, `depthsuspend`, `townaura`, `footstep`, `snowshovel`,
//! `droplet` and `take` return [`None`]. Two are absent from the modern
//! registry outright (`townaura`, `footstep` — both already documented as
//! vestigial, never sent by vanilla even in 1.12). For the rest, the
//! disassembly search is negative evidence, not silence: the only two
//! plausibly-related modern keys (`mycelium`, `underwater`) have **zero**
//! server-side references anywhere in the 1.13.2 jar, meaning the real
//! server never networks them (they are client-simulated ambient effects),
//! and `droplet`/`take` (`WATER_DROP`/`ITEM_TAKE`) have no surviving modern
//! key at all. With no call site to confirm a direction and both hypotheses
//! equally unfalsifiable, guessing `suspended → mycelium` over
//! `suspended → underwater` (or the reverse) would be exactly the "guessed
//! rename table is worse than none" case this module's own doc exists to
//! avoid — so these decode as an explicit miss rather than a coin flip.

/// Resolves a legacy 1.12.2 `world_particles` numeric particle id to its
/// canonical `minecraft:*` particle-type identifier.
///
/// Returns `None` for an id outside the legacy `0..=48` range, or one of the
/// seven ids with no confirmed modern key (see the module docs).
#[must_use]
pub fn particle_key(id: i32) -> Option<&'static str> {
    Some(match id {
        0 => "minecraft:poof",             // EXPLOSION_NORMAL — verified, Explosion's per-block burst
        1 => "minecraft:explosion",        // EXPLOSION_LARGE — verified, Explosion size < 2
        2 => "minecraft:explosion_emitter", // EXPLOSION_HUGE — verified, Explosion size >= 2 && smoking
        3 => "minecraft:firework",         // FIREWORKS_SPARK — identity
        4 => "minecraft:bubble",           // WATER_BUBBLE — identity
        5 => "minecraft:splash",           // WATER_SPLASH — identity
        6 => "minecraft:fishing",          // WATER_WAKE — forced (fishing-bobber ripple)
        7 => return None,                  // SUSPENDED — no confirmed modern key, see module docs
        8 => return None,                  // SUSPENDED_DEPTH — no confirmed modern key, see module docs
        9 => "minecraft:crit",             // CRIT — identity, confirmed server-side reference
        10 => "minecraft:enchanted_hit",   // CRIT_MAGIC — forced (only remaining candidate)
        11 => "minecraft:smoke",           // SMOKE_NORMAL — verified, Explosion's secondary smoke
        12 => "minecraft:large_smoke",     // SMOKE_LARGE — identity
        13 => "minecraft:effect",          // SPELL — forced (effect-family, 1:1 elimination)
        14 => "minecraft:instant_effect",  // SPELL_INSTANT — forced
        15 => "minecraft:entity_effect",   // SPELL_MOB — forced, corroborated by a shared call site with SPELL_MOB_AMBIENT
        16 => "minecraft:ambient_entity_effect", // SPELL_MOB_AMBIENT — forced, corroborated
        17 => "minecraft:witch",           // SPELL_WITCH — forced
        18 => "minecraft:dripping_water",  // DRIP_WATER — identity
        19 => "minecraft:dripping_lava",   // DRIP_LAVA — identity
        20 => "minecraft:angry_villager",  // VILLAGER_ANGRY — identity
        21 => "minecraft:happy_villager",  // VILLAGER_HAPPY — identity
        22 => return None,                 // TOWN_AURA — absent from the modern registry; vestigial in 1.12 too
        23 => "minecraft:note",            // NOTE — identity
        24 => "minecraft:portal",          // PORTAL — identity
        25 => "minecraft:enchant",         // ENCHANTMENT_TABLE — forced (only remaining candidate)
        26 => "minecraft:flame",           // FLAME — identity
        27 => "minecraft:lava",            // LAVA — identity
        28 => return None,                 // FOOTSTEP — absent from the modern registry; vestigial in 1.12 too
        29 => "minecraft:cloud",           // CLOUD — identity
        30 => "minecraft:dust",            // REDSTONE — forced (redstone dust particle, now colour-parameterized)
        31 => "minecraft:item_snowball",   // SNOWBALL — forced (name match, 1:1 elimination)
        32 => return None,                 // SNOW_SHOVEL — no confirmed modern key, see module docs
        33 => "minecraft:item_slime",      // SLIME — forced (name match, 1:1 elimination)
        34 => "minecraft:heart",           // HEART — identity
        35 => "minecraft:barrier",         // BARRIER — identity
        36 => "minecraft:item",            // ITEM_CRACK — verified via disassembly (item-stack debris)
        37 => "minecraft:block",           // BLOCK_CRACK — verified via disassembly (block-break debris)
        38 => "minecraft:block",           // BLOCK_DUST — verified via disassembly; merged into `block` in 1.13
        39 => return None,                 // WATER_DROP — no surviving modern key
        40 => return None,                 // ITEM_TAKE — no surviving modern key
        41 => "minecraft:elder_guardian",  // MOB_APPEARANCE — forced (only remaining candidate)
        42 => "minecraft:dragon_breath",   // DRAGON_BREATH — identity
        43 => "minecraft:end_rod",         // END_ROD — identity
        44 => "minecraft:damage_indicator", // DAMAGE_INDICATOR — identity
        45 => "minecraft:sweep_attack",    // SWEEP_ATTACK — identity
        46 => "minecraft:falling_dust",    // FALLING_DUST — identity
        47 => "minecraft:totem_of_undying", // TOTEM — forced (only remaining candidate)
        48 => "minecraft:spit",            // SPIT — identity
        _ => return None,
    })
}

/// Legacy particle ids that carry extra type-specific `varint` data after the
/// fixed packet prefix (`vendor/minecraft-data`'s `packet_world_particles`
/// `data` switch): `iconcrack` (item id, item damage), `blockcrack` and
/// `blockdust` (one legacy block-state varint each). Every other id carries
/// none.
#[must_use]
pub fn extra_varint_count(id: i32) -> usize {
    match id {
        36 => 2,
        37 | 38 => 1,
        _ => 0,
    }
}
