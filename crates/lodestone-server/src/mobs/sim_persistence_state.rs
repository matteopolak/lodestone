//! Per-mob saved state beyond pose and health: what a world reload must give back
//! to a villager, a pet, a leashed animal or a named mob.
//!
//! Field names and value encodings are the ones a vanilla server writes into an
//! entity record (checked against the committed vanilla entity corpus in
//! `tests/entity_nbt_vanilla_oracle.rs`), so the same [`SavedEntity::extra`] list
//! round-trips through the Anvil import and export path.
//!
//! The split is *owned* versus *carried*: a field the sim models is decoded onto
//! [`SimMob`] state and re-encoded from it on save; every other field is carried
//! verbatim in [`SimMob::passthrough`]. [`OWNED_FIELDS`] is the dividing line.

use lodestone_core::Nbt;

use super::*;
use crate::entity_record::{field, read_uuid, uuid_to_ints};

/// Saved fields decoded onto sim state. They are removed from the verbatim
/// carry so a stale copy can never be written beside the live value.
///
/// `VillagerData` and `Brain` are deliberately absent: the sim owns only part of
/// each (profession and level; three of the memories), so the whole compound is
/// carried and the owned parts are overwritten on save.
pub(super) const OWNED_FIELDS: &[&str] = &[
    "anger_end_time",
    "angry_at",
    "Age",
    "AgeLocked",
    "InLove",
    "PersistenceRequired",
    "Owner",
    "Sitting",
    "Tame",
    "Temper",
    "leash",
    "Xp",
    "Offers",
    "Gossips",
    "LastGossipDecay",
    "LastRestock",
    "RestocksToday",
    "VillagerDataFinalized",
    "HasNectar",
    "HasStung",
    "hive_pos",
    "flower_pos",
    "TicksSincePollination",
    "CannotEnterHiveTicks",
    "CropsGrownSincePollination",
    "home_pos",
    "home_radius",
];

/// Species with a persistent grudge: the ones whose saves carry
/// `anger_end_time`.
fn is_neutral_species(species: &str) -> bool {
    matches!(
        species,
        "bee" | "wolf" | "enderman" | "zombified_piglin" | "iron_golem" | "polar_bear"
    )
}

/// Species whose tamed state is vanilla's horse-family `Tame`/`Temper` pair.
fn is_horse_family(species: &str) -> bool {
    matches!(
        species,
        "horse" | "donkey" | "mule" | "skeleton_horse" | "zombie_horse" | "llama" | "trader_llama"
    )
}

fn byte(value: bool) -> Nbt {
    Nbt::Byte(i8::from(value))
}

fn int_array_pos(pos: BlockPos) -> Nbt {
    Nbt::IntArray(vec![pos.x, pos.y, pos.z])
}

fn read_pos(nbt: Option<&Nbt>) -> Option<BlockPos> {
    match nbt {
        Some(Nbt::IntArray(v)) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
        _ => None,
    }
}

fn compound(fields: Vec<(&str, Nbt)>) -> Nbt {
    Nbt::Compound(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn list_of_compounds(items: Vec<Nbt>) -> Nbt {
    Nbt::List {
        element_type: lodestone_core::NbtTag::Compound,
        elements: items,
    }
}

fn set_field(fields: &mut Vec<(String, Nbt)>, key: &str, value: Nbt) {
    match fields.iter_mut().find(|(name, _)| name == key) {
        Some(slot) => slot.1 = value,
        None => fields.push((key.to_owned(), value)),
    }
}

fn compound_fields(nbt: Option<&Nbt>) -> Vec<(String, Nbt)> {
    match nbt {
        Some(Nbt::Compound(fields)) => fields.clone(),
        _ => Vec::new(),
    }
}

fn stack(item: &str, count: i32) -> Nbt {
    compound(vec![("count", Nbt::Int(count)), ("id", Nbt::String(item.to_owned()))])
}

fn int_of(nbt: Option<&Nbt>) -> Option<i32> {
    match nbt {
        Some(Nbt::Int(v)) => Some(*v),
        Some(Nbt::Short(v)) => Some(i32::from(*v)),
        Some(Nbt::Byte(v)) => Some(i32::from(*v)),
        _ => None,
    }
}

fn flag_of(nbt: Option<&Nbt>) -> bool {
    int_of(nbt).is_some_and(|v| v != 0)
}

fn stack_id(nbt: Option<&Nbt>) -> Option<String> {
    match nbt.and_then(|stack| field(stack, "id")) {
        Some(Nbt::String(id)) => Some(id.clone()),
        _ => None,
    }
}

impl<'w> MobSim<'w> {
    /// The runtime id's uuid, for writing a mob-to-mob reference.
    fn uuid_of_mob(&self, id: i32) -> Option<Uuid> {
        self.mobs.iter().find(|m| m.id == id).map(|m| m.uuid)
    }

    /// Every saved field this sim owns for `mob`, followed by the carried ones.
    pub(super) fn mob_state_fields(&self, mob: &SimMob<'_>) -> Vec<(String, Nbt)> {
        let mut fields: Vec<(String, Nbt)> = Vec::new();
        let species = mob.entity_type.path();

        // Only a set flag is stored, so a restored mob is never made
        // despawnable by a `0` it was saved with.
        if mob.persistence_required {
            fields.push(("PersistenceRequired".to_owned(), byte(true)));
        }

        let owner_uuid = match mob.owner {
            Some(MobOwner::Player(uuid)) => Some(uuid),
            Some(MobOwner::Mob(id)) => self.uuid_of_mob(id),
            None => None,
        };
        if let Some(uuid) = owner_uuid {
            fields.push(("Owner".to_owned(), Nbt::IntArray(uuid_to_ints(uuid))));
        }
        // Vanilla writes the sitting order for every tameable species, true or not.
        // Neutral species always write their grudge deadline (`-1` for none)
        // and, when the offender is known, its uuid.
        if is_neutral_species(species) {
            let end = mob.anger.map_or(-1, |anger| anger.end_time as i64);
            fields.push(("anger_end_time".to_owned(), Nbt::Long(end)));
            if let Some(uuid) = mob.anger.and_then(|anger| anger.attacker) {
                fields.push(("angry_at".to_owned(), Nbt::IntArray(uuid_to_ints(uuid))));
            }
        }
        if mob.love_time() > 0 {
            fields.push(("InLove".to_owned(), Nbt::Int(mob.love_time())));
        }
        if mob.ordered_to_sit || matches!(species, "wolf" | "cat" | "parrot") {
            fields.push(("Sitting".to_owned(), byte(mob.ordered_to_sit)));
        }
        if mob.tame && is_horse_family(species) {
            fields.push(("Tame".to_owned(), byte(true)));
            if mob.temper != 0 {
                fields.push(("Temper".to_owned(), Nbt::Int(mob.temper)));
            }
        }

        let look = &mob.appearance;
        match species {
            "sheep" => {
                fields.push(("Color".to_owned(), Nbt::Byte(look.wool as i8)));
                fields.push(("Sheared".to_owned(), byte(look.sheared)));
            }
            "wolf" | "cat" => fields.push(("CollarColor".to_owned(), Nbt::Byte(look.collar as i8))),
            _ => {}
        }
        if let Some(name) = &look.custom_name {
            fields.push(("CustomName".to_owned(), name.clone()));
            if look.name_visible {
                fields.push(("CustomNameVisible".to_owned(), byte(true)));
            }
        }
        if let (Some(field), Some(variant)) = (appearance::variant_field(species), &look.variant) {
            fields.push((field.to_owned(), variant.to_nbt()));
        }

        match mob.leash_holder {
            Some(LeashHolder::Player(uuid)) => fields.push((
                "leash".to_owned(),
                compound(vec![("UUID", Nbt::IntArray(uuid_to_ints(uuid)))]),
            )),
            Some(LeashHolder::Mob(id)) => {
                if let Some(uuid) = self.uuid_of_mob(id) {
                    fields.push((
                        "leash".to_owned(),
                        compound(vec![("UUID", Nbt::IntArray(uuid_to_ints(uuid)))]),
                    ));
                }
            }
            Some(LeashHolder::Fence(pos)) => fields.push(("leash".to_owned(), int_array_pos(pos))),
            None => {}
        }

        if species == "villager" {
            self.villager_fields(mob, &mut fields);
        }
        if species == "bee" {
            let bee = mob.mob.bee_state();
            fields.push(("HasNectar".to_owned(), byte(bee.has_nectar)));
            fields.push(("HasStung".to_owned(), byte(mob.stung_at.is_some())));
            fields.push(("TicksSincePollination".to_owned(), Nbt::Int(bee.ticks_without_nectar)));
            fields.push(("CannotEnterHiveTicks".to_owned(), Nbt::Int(bee.stay_out_ticks)));
            fields.push(("CropsGrownSincePollination".to_owned(), Nbt::Int(bee.crops_grown)));
            for (name, cell) in [("hive_pos", bee.hive), ("flower_pos", bee.flower)] {
                if let Some((x, y, z)) = cell {
                    fields.push((name.to_owned(), int_array_pos(BlockPos::new(x, y, z))));
                }
            }
        }

        if let Some((at, radius)) = mob.mob.restriction() {
            fields.push(("home_radius".to_owned(), Nbt::Int(radius)));
            fields.push(("home_pos".to_owned(), int_array_pos(at)));
        }

        for (name, value) in &mob.passthrough {
            if !fields.iter().any(|(existing, _)| existing == name) {
                fields.push((name.clone(), value.clone()));
            }
        }
        fields
    }

    fn villager_fields(&self, mob: &SimMob<'_>, fields: &mut Vec<(String, Nbt)>) {
        // Merge into the carried compounds so the biome `type` and any brain
        // memory the sim does not model survive.
        let carried = |name: &str| {
            mob.passthrough
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value)
        };
        let mut data = compound_fields(carried("VillagerData"));
        set_field(
            &mut data,
            "profession",
            Nbt::String(format!("minecraft:{}", mob.profession.path())),
        );
        set_field(&mut data, "level", Nbt::Int(mob.villager_level));
        if !data.iter().any(|(k, _)| k == "type") {
            data.push(("type".to_owned(), Nbt::String("minecraft:plains".to_owned())));
        }
        fields.retain(|(name, _)| name != "VillagerData");
        fields.push(("VillagerData".to_owned(), Nbt::Compound(data)));
        fields.push(("Xp".to_owned(), Nbt::Int(mob.villager_xp)));
        fields.push(("VillagerDataFinalized".to_owned(), byte(true)));

        if let Some((_, _, trades)) = &mob.trades {
            let recipes = trades
                .offers
                .iter()
                .map(|offer| {
                    let record = &offer.record;
                    let mut recipe = vec![
                        ("maxUses", Nbt::Int(record.max_uses)),
                        ("buy", stack(record.wants_item, record.wants_count)),
                        ("sell", stack(record.gives_item, record.gives_count)),
                        ("xp", Nbt::Int(record.xp)),
                        ("priceMultiplier", Nbt::Float(record.price_multiplier)),
                        ("demand", Nbt::Int(offer.demand)),
                    ];
                    if offer.uses != 0 {
                        recipe.push(("uses", Nbt::Int(offer.uses)));
                    }
                    if offer.special_price_diff != 0 {
                        recipe.push(("specialPrice", Nbt::Int(offer.special_price_diff)));
                    }
                    if let Some((item, count)) = record.wants_b {
                        recipe.push(("buyB", stack(item, count)));
                    }
                    compound(recipe)
                })
                .collect();
            fields.push((
                "Offers".to_owned(),
                compound(vec![("Recipes", list_of_compounds(recipes))]),
            ));
            fields.push((
                "LastRestock".to_owned(),
                Nbt::Long(trades.restock.last_restock_game_time),
            ));
            fields.push((
                "RestocksToday".to_owned(),
                Nbt::Int(trades.restock.number_of_restocks_today),
            ));
        }

        let gossips: Vec<Nbt> = mob
            .gossip
            .iter()
            .map(|(target, kind, count)| {
                compound(vec![
                    ("Target", Nbt::IntArray(uuid_to_ints(target))),
                    ("Type", Nbt::String(kind.name().to_owned())),
                    ("Value", Nbt::Int(count)),
                ])
            })
            .collect();
        fields.push(("Gossips".to_owned(), list_of_compounds(gossips)));
        if let Some(tick) = mob.last_gossip_decay_tick {
            fields.push(("LastGossipDecay".to_owned(), Nbt::Long(tick as i64)));
        }

        // The job site, home bed and meeting point are the three brain
        // memories the sim holds; every other memory is carried untouched.
        let mut brain = compound_fields(carried("Brain"));
        let mut memories = compound_fields(field_of(&brain, "memories"));
        let dimension = |memories: &[(String, Nbt)], key: &str| {
            memories
                .iter()
                .find(|(name, _)| name == key)
                .and_then(|(_, memory)| field(memory, "value"))
                .and_then(|value| field(value, "dimension"))
                .cloned()
                .unwrap_or_else(|| Nbt::String("minecraft:overworld".to_owned()))
        };
        for (key, pos) in [
            ("minecraft:job_site", mob.workstation),
            ("minecraft:home", mob.bed),
            ("minecraft:meeting_point", mob.meeting_point),
        ] {
            let dim = dimension(&memories, key);
            memories.retain(|(name, _)| name != key);
            if let Some(pos) = pos {
                memories.push((
                    key.to_owned(),
                    compound(vec![(
                        "value",
                        compound(vec![("pos", int_array_pos(pos)), ("dimension", dim)]),
                    )]),
                ));
            }
        }
        set_field(&mut brain, "memories", Nbt::Compound(memories));
        fields.retain(|(name, _)| name != "Brain");
        fields.push(("Brain".to_owned(), Nbt::Compound(brain)));
    }

    /// Applies a saved record's modeled fields to the freshly spawned mob `id`
    /// and stores the rest for the next save. Mob-to-mob references (owner,
    /// leash holder) are returned for [`resolve_references`](Self::resolve_references),
    /// which runs once the whole batch exists.
    pub(super) fn restore_mob_state(
        &mut self,
        id: i32,
        extra: &[(String, Nbt)],
    ) -> PendingReferences {
        let mut pending = PendingReferences { id, owner: None, leash: None };
        let world = self.world;
        let restock_clock = self.tick_count as i64;
        let Some(index) = self.mobs.iter().position(|m| m.id == id) else {
            return pending;
        };
        let species = self.mobs[index].entity_type.path().to_owned();
        let get = |name: &str| {
            extra.iter().find(|(key, _)| key == name).map(|(_, value)| value)
        };

        let named = get("CustomName").is_some();
        {
            let mob = &mut self.mobs[index];
            let owned_here = appearance::owned_fields(&species);
            mob.passthrough = extra
                .iter()
                .filter(|(name, _)| {
                    !OWNED_FIELDS.contains(&name.as_str()) && !owned_here.contains(&name.as_str())
                })
                .cloned()
                .collect();
            let look = &mut mob.appearance;
            if let Some(color) = int_of(get("Color")).filter(|_| species == "sheep") {
                look.wool = (color & 0x0F) as u8;
            }
            if species == "sheep" {
                look.sheared = flag_of(get("Sheared"));
            }
            if let Some(color) = int_of(get("CollarColor")) {
                look.collar = (color & 0x0F) as u8;
            }
            if let Some(name) = get("CustomName") {
                look.custom_name = Some(name.clone());
            }
            look.name_visible = flag_of(get("CustomNameVisible"));
            if let Some(variant) = appearance::variant_field(&species)
                .and_then(|field| get(field))
                .and_then(appearance::MobVariant::from_nbt)
            {
                look.variant = Some(variant);
            }
            // A name tag is what makes vanilla stop despawning a mob.
            if flag_of(get("PersistenceRequired")) || named {
                mob.set_persistence_required(true);
            }
            // A saved deadline is absolute game time; this sim's clock is its
            // own tick count, so a deadline beyond the longest grudge a hit can
            // start is clamped rather than outliving a clock that restarted.
            if let Some(Nbt::Long(end)) = get("anger_end_time")
                && *end >= 0
                && is_neutral_species(&species)
            {
                let now = restock_clock as u64;
                let end = (*end as u64).min(now + ANGER_TICKS.1);
                if end > now {
                    mob.anger = Some(Anger {
                        end_time: end,
                        target: None,
                        attacker: read_uuid(get("angry_at")),
                    });
                }
            }
            if let Some(ticks) = int_of(get("InLove")) {
                mob.mob.set_love_time(ticks);
            }
            if flag_of(get("Sitting")) {
                mob.set_ordered_to_sit(true);
            }
            if is_horse_family(&species) {
                if flag_of(get("Tame")) {
                    mob.tame = true;
                    mob.mob.set_tame(true);
                }
                if let Some(temper) = int_of(get("Temper")) {
                    mob.temper = temper;
                }
            }
            match get("leash") {
                Some(pos @ Nbt::IntArray(_)) => {
                    if let Some(pos) = read_pos(Some(pos)) {
                        mob.leash_holder = Some(LeashHolder::Fence(pos));
                    }
                }
                Some(holder) => pending.leash = read_uuid(field(holder, "UUID")),
                None => {}
            }
        }
        pending.owner = read_uuid(get("Owner"));

        if let (Some(at), Some(radius)) = (read_pos(get("home_pos")), int_of(get("home_radius")))
            && radius >= 0
        {
            self.mobs[index].mob.set_restriction(Some((at, radius)));
        }

        if species == "bee" {
            let cell = |name: &str| read_pos(get(name)).map(|p| (p.x, p.y, p.z));
            let mob = &mut self.mobs[index];
            if flag_of(get("HasStung")) {
                mob.stung_at = Some(restock_clock as u64);
            }
            let bee = mob.mob.bee_state_mut();
            bee.has_nectar = flag_of(get("HasNectar"));
            bee.ticks_without_nectar = int_of(get("TicksSincePollination")).unwrap_or(0).max(0);
            bee.stay_out_ticks = int_of(get("CannotEnterHiveTicks")).unwrap_or(0).max(0);
            bee.crops_grown = int_of(get("CropsGrownSincePollination")).unwrap_or(0).max(0);
            bee.hive = cell("hive_pos");
            bee.flower = cell("flower_pos");
        }

        if species == "villager" {
            self.restore_villager(index, extra, world, restock_clock);
        }
        pending
    }

    fn restore_villager(
        &mut self,
        index: usize,
        extra: &[(String, Nbt)],
        world: &'w ChunkWorld,
        clock: i64,
    ) {
        let get = |name: &str| {
            extra.iter().find(|(key, _)| key == name).map(|(_, value)| value)
        };
        let data = get("VillagerData");
        let profession = match data.and_then(|d| field(d, "profession")) {
            Some(Nbt::String(id)) => id
                .strip_prefix("minecraft:")
                .and_then(villager::Profession::from_path)
                .unwrap_or_default(),
            _ => villager::Profession::None,
        };
        let level = int_of(data.and_then(|d| field(d, "level"))).unwrap_or(1).clamp(1, 5);
        let xp = int_of(get("Xp")).unwrap_or(0).max(0);
        let memory_pos = |key: &str| {
            let memories = get("Brain").and_then(|brain| field(brain, "memories"))?;
            let value = field(memories, key).and_then(|memory| field(memory, "value"))?;
            read_pos(field(value, "pos"))
        };
        let job_site = memory_pos("minecraft:job_site");
        let home = memory_pos("minecraft:home");
        let meeting = memory_pos("minecraft:meeting_point");

        let mob = &mut self.mobs[index];
        mob.profession = profession;
        mob.villager_level = level;
        mob.villager_xp = xp;
        // Job-site, bed and bell claims live in the native point-of-interest
        // ledgers; without them the browser build keeps the profession and
        // searches again.
        {
            // The claim is re-acquired, not trusted: the saved job site only counts
            // if the block there still hands out this profession and a ticket is
            // free. A villager whose station is unloaded or gone keeps its
            // profession and searches again from `tick_villager_professions`.
            if let Some(pos) = job_site
                && profession.has_job_site()
                && villager::claim_workstation_at(pos, profession, world, &mut self.workstation_claims)
            {
                mob.workstation = Some(pos);
            }
            if let Some(pos) = home
                && villager::is_bed_state(world.block_state_id(pos.x, pos.y, pos.z))
                && self.bed_claims.try_claim(pos)
            {
                mob.bed = Some(pos);
            }
            if let Some(pos) = meeting
                && villager::is_bell_state(world.block_state_id(pos.x, pos.y, pos.z))
                && self.bell_claims.try_claim(pos)
            {
                mob.meeting_point = Some(pos);
            }
        }

        if let Some(Nbt::List { elements, .. }) = get("Gossips") {
            for entry in elements {
                let target = read_uuid(field(entry, "Target"));
                let kind = match field(entry, "Type") {
                    Some(Nbt::String(name)) => villager::gossip::GossipType::from_name(name),
                    _ => None,
                };
                let value = int_of(field(entry, "Value"));
                if let (Some(target), Some(kind), Some(value)) = (target, kind, value) {
                    mob.gossip.add(target, kind, value);
                }
            }
        }
        // A saved decay stamp from a longer-running clock would postpone the
        // next decay past this sim's clock, so it is clamped to now.
        if let Some(Nbt::Long(tick)) = get("LastGossipDecay") {
            mob.last_gossip_decay_tick = Some((*tick).clamp(0, clock) as u64);
        }

        if let Some(trades) = mob.ensure_trades() {
            if let Some(Nbt::Compound(_)) = get("Offers") {
                let recipes: &[Nbt] = match get("Offers").and_then(|o| field(o, "Recipes")) {
                    Some(Nbt::List { elements, .. }) => elements,
                    _ => &[],
                };
                let mut taken = vec![false; trades.offers.len()];
                for recipe in recipes {
                    let buy = stack_id(field(recipe, "buy"));
                    let sell = stack_id(field(recipe, "sell"));
                    let slot = trades.offers.iter().enumerate().position(|(i, offer)| {
                        !taken[i]
                            && buy.as_deref() == Some(offer.record.wants_item)
                            && sell.as_deref() == Some(offer.record.gives_item)
                    });
                    if let Some(i) = slot {
                        taken[i] = true;
                        let offer = &mut trades.offers[i];
                        offer.uses = int_of(field(recipe, "uses")).unwrap_or(0).max(0);
                        offer.demand = int_of(field(recipe, "demand")).unwrap_or(0);
                        offer.special_price_diff =
                            int_of(field(recipe, "specialPrice")).unwrap_or(0);
                    }
                }
            }
            if let Some(Nbt::Long(last)) = get("LastRestock") {
                trades.restock.last_restock_game_time = (*last).min(clock);
            }
            if let Some(count) = int_of(get("RestocksToday")) {
                trades.restock.number_of_restocks_today = count.max(0);
            }
        }
    }

    /// Resolves the uuid references a batch of restored records carried: an
    /// owner or leash holder that names a live mob becomes a mob reference,
    /// anything else is a player (players are the only other uuid-addressed
    /// owner, and one that is offline stays a uuid until it reconnects).
    pub(super) fn resolve_references(&mut self, pending: Vec<PendingReferences>) {
        for refs in pending {
            let owner = refs.owner.map(|uuid| {
                self.mobs
                    .iter()
                    .find(|m| m.uuid == uuid)
                    .map_or(MobOwner::Player(uuid), |m| MobOwner::Mob(m.id))
            });
            let leash = refs.leash.map(|uuid| {
                self.mobs
                    .iter()
                    .find(|m| m.uuid == uuid)
                    .map_or(LeashHolder::Player(uuid), |m| LeashHolder::Mob(m.id))
            });
            let Some(mob) = self.mobs.iter_mut().find(|m| m.id == refs.id) else {
                continue;
            };
            if let Some(owner) = owner {
                mob.tame(owner);
            }
            if let Some(holder) = leash {
                mob.leash_holder = Some(holder);
            }
        }
        // A restored grudge knows its offender only by uuid; point it at where
        // that offender is now, when it is already in the world.
        let positions = self.grudge_positions();
        for (mob, position) in self.mobs.iter_mut().zip(positions) {
            if let (Some(anger), Some(position)) = (mob.anger.as_mut(), position) {
                anger.target = Some(position);
            }
        }
    }
}

/// Uuid references from one restored record, resolved after the batch exists.
#[derive(Debug)]
pub(super) struct PendingReferences {
    id: i32,
    owner: Option<Uuid>,
    leash: Option<Uuid>,
}

fn field_of<'a>(fields: &'a [(String, Nbt)], key: &str) -> Option<&'a Nbt> {
    fields.iter().find(|(name, _)| name == key).map(|(_, value)| value)
}
