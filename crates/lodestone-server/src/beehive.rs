//! Beehive and bee nest block-entity state: the bees inside and when they leave.
//!
//! # What it is
//!
//! A hive stores up to three bees as saved entity records. Each waits out a
//! minimum stay (600 ticks, or 2400 when it carried nectar in), then comes out
//! if the hive's front is open and bees are not being kept in. A bee that
//! brought nectar raises the hive's honey level on the way out.
//!
//! # How it works
//!
//! [`Beehive::tick`] advances every occupant's clock and returns the ones that
//! leave. The stay test is `ticks_in_hive > min_ticks_in_hive` read before the
//! clock advances, so a bee that cannot leave this tick (night, rain, a blocked
//! front) is retried every tick until it can. The caller supplies those two
//! gates, turns each [`Released`] into a live bee, and writes the honey level
//! with [`honey_after_delivery`].
//!
//! A bee entering brings its saved entity compound (its `id` and the state
//! `MobSim` saves for a bee); the hive never looks inside except for
//! `HasNectar`.
//!
//! # How to change it
//!
//! The wiring is `crate::tick::run_tick_loop`: it ticks hives through
//! `BlockEntityRegistry::tick_hives`, feeds their occupancy to `MobSim`, and
//! applies `MobSim::take_hive_entries`. Saved form is `{bees: [{entity_data,
//! ticks_in_hive, min_ticks_in_hive}], flower_pos}`.
//!
//! # Not modelled
//!
//! Smoke sedation, the work and entry sounds, and fire driving bees out of a
//! burning hive (a released-in-emergency path exists as [`Beehive::release_all`]
//! for the caller that breaks the block).

use lodestone_core::{Nbt, NbtTag};
use lodestone_model::BlockPos;

/// Most bees a hive holds.
pub const MAX_OCCUPANTS: usize = 3;
/// Minimum stay of a bee that brought nectar.
pub const MIN_STAY_WITH_NECTAR: i32 = 2400;
/// Minimum stay of a bee that brought none.
pub const MIN_STAY_WITHOUT_NECTAR: i32 = 600;
/// The honey level at which a hive is full.
pub const MAX_HONEY_LEVEL: u8 = 5;

/// One bee in a hive.
#[derive(Debug, Clone, PartialEq)]
pub struct Occupant {
    /// The bee's saved entity compound, including its `id`.
    pub entity_data: Nbt,
    /// Ticks it has been inside.
    pub ticks_in_hive: i32,
    /// Ticks it must stay before it may leave.
    pub min_ticks_in_hive: i32,
}

impl Occupant {
    /// A bee that just entered, with the minimum stay its nectar sets.
    #[must_use]
    pub fn entering(entity_data: Nbt) -> Self {
        let min = if flag(&entity_data, "HasNectar") { MIN_STAY_WITH_NECTAR } else { MIN_STAY_WITHOUT_NECTAR };
        Self { entity_data, ticks_in_hive: 0, min_ticks_in_hive: min }
    }

    /// Whether it carried nectar in.
    #[must_use]
    pub fn has_nectar(&self) -> bool {
        flag(&self.entity_data, "HasNectar")
    }
}

fn flag(compound: &Nbt, key: &str) -> bool {
    match compound {
        Nbt::Compound(fields) => fields.iter().any(|(k, v)| k == key && matches!(v, Nbt::Byte(b) if *b != 0)),
        _ => false,
    }
}

/// A bee leaving a hive.
#[derive(Debug, Clone, PartialEq)]
pub struct Released {
    /// The bee, with its final time inside.
    pub occupant: Occupant,
    /// Whether it delivered nectar, which raises the honey level.
    pub honey_delivered: bool,
}

/// The bees in one hive and the bloom they last reported.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Beehive {
    occupants: Vec<Occupant>,
    flower: Option<BlockPos>,
}

impl Beehive {
    /// A hive restored from its saved parts.
    #[must_use]
    pub fn restore(occupants: Vec<Occupant>, flower: Option<BlockPos>) -> Self {
        Self { occupants, flower }
    }

    /// The bees inside.
    #[must_use]
    pub fn occupants(&self) -> &[Occupant] {
        &self.occupants
    }

    /// The bloom the hive remembers, handed to bees that leave without one.
    #[must_use]
    pub fn flower(&self) -> Option<BlockPos> {
        self.flower
    }

    /// Whether no more bees fit.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.occupants.len() >= MAX_OCCUPANTS
    }

    /// Takes a bee in. A bee that knows a bloom replaces the hive's with a
    /// coin flip, or outright when the hive knows none. Returns `false` when
    /// the hive is full.
    pub fn add(&mut self, occupant: Occupant, bee_flower: Option<BlockPos>, coin: bool) -> bool {
        if self.is_full() {
            return false;
        }
        self.occupants.push(occupant);
        if bee_flower.is_some() && (self.flower.is_none() || coin) {
            self.flower = bee_flower;
        }
        true
    }

    /// Advances every occupant one tick and returns those that leave.
    ///
    /// `stay_in` keeps bees inside (night, rain); `front_blocked` keeps them in
    /// while something solid stands in front of the hive.
    pub fn tick(&mut self, stay_in: bool, front_blocked: bool) -> Vec<Released> {
        let mut out = Vec::new();
        let mut kept = Vec::with_capacity(self.occupants.len());
        for mut occupant in std::mem::take(&mut self.occupants) {
            let due = occupant.ticks_in_hive > occupant.min_ticks_in_hive;
            occupant.ticks_in_hive += 1;
            if due && !stay_in && !front_blocked {
                let honey_delivered = occupant.has_nectar();
                out.push(Released { occupant, honey_delivered });
            } else {
                kept.push(occupant);
            }
        }
        self.occupants = kept;
        out
    }

    /// Releases every occupant at once, whatever the hour or the front: the
    /// hive was broken or burned. None of them delivers honey.
    pub fn release_all(&mut self) -> Vec<Released> {
        std::mem::take(&mut self.occupants)
            .into_iter()
            .map(|occupant| Released { occupant, honey_delivered: false })
            .collect()
    }

    /// The hive's saved fields.
    #[must_use]
    pub fn to_nbt_fields(&self) -> Vec<(String, Nbt)> {
        let bees = self
            .occupants
            .iter()
            .map(|o| {
                Nbt::Compound(vec![
                    ("entity_data".to_owned(), o.entity_data.clone()),
                    ("ticks_in_hive".to_owned(), Nbt::Int(o.ticks_in_hive)),
                    ("min_ticks_in_hive".to_owned(), Nbt::Int(o.min_ticks_in_hive)),
                ])
            })
            .collect::<Vec<_>>();
        let mut fields = vec![(
            "bees".to_owned(),
            Nbt::List {
                element_type: if bees.is_empty() { NbtTag::End } else { NbtTag::Compound },
                elements: bees,
            },
        )];
        if let Some(pos) = self.flower {
            fields.push(("flower_pos".to_owned(), Nbt::IntArray(vec![pos.x, pos.y, pos.z])));
        }
        fields
    }

    /// Reads a hive from its save compound, skipping malformed occupants and
    /// any beyond the capacity.
    #[must_use]
    pub fn from_nbt(save: &Nbt) -> Self {
        let Nbt::Compound(fields) = save else { return Self::default() };
        let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
        let int = |compound: &Nbt, key: &str| match compound {
            Nbt::Compound(f) => f.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
                Nbt::Int(i) => Some(*i),
                Nbt::Short(i) => Some(i32::from(*i)),
                Nbt::Byte(i) => Some(i32::from(*i)),
                _ => None,
            }),
            _ => None,
        };
        let mut occupants = Vec::new();
        if let Some(Nbt::List { elements, .. }) = get("bees") {
            for bee in elements {
                let Nbt::Compound(inner) = bee else { continue };
                let Some((_, entity_data @ Nbt::Compound(_))) = inner.iter().find(|(k, _)| k == "entity_data") else {
                    continue;
                };
                occupants.push(Occupant {
                    entity_data: entity_data.clone(),
                    ticks_in_hive: int(bee, "ticks_in_hive").unwrap_or(0),
                    min_ticks_in_hive: int(bee, "min_ticks_in_hive").unwrap_or(MIN_STAY_WITHOUT_NECTAR),
                });
            }
        }
        occupants.truncate(MAX_OCCUPANTS);
        let flower = match get("flower_pos") {
            Some(Nbt::IntArray(v)) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
            _ => None,
        };
        Self { occupants, flower }
    }
}

/// The honey level after a bee delivers nectar: one more, or two on a 1 in 100
/// roll (`roll_hundred == 0`), never past [`MAX_HONEY_LEVEL`]. A full hive is
/// unchanged.
#[must_use]
pub fn honey_after_delivery(level: u8, roll_hundred: u32) -> u8 {
    if level >= MAX_HONEY_LEVEL {
        return level;
    }
    let mut increase = if roll_hundred == 0 { 2 } else { 1 };
    if level + increase > MAX_HONEY_LEVEL {
        increase -= 1;
    }
    level + increase
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bee(nectar: bool) -> Nbt {
        Nbt::Compound(vec![
            ("id".to_owned(), Nbt::String("minecraft:bee".to_owned())),
            ("HasNectar".to_owned(), Nbt::Byte(i8::from(nectar))),
        ])
    }

    #[test]
    fn a_nectarless_bee_leaves_after_601_ticks_and_one_with_nectar_after_2401() {
        for (nectar, stay) in [(false, 600), (true, 2400)] {
            let mut hive = Beehive::default();
            assert!(hive.add(Occupant::entering(bee(nectar)), None, false));
            // The clock reads `ticks > min` before it advances, so the bee is
            // due on the tick after the counter first exceeds `min`.
            for tick in 0..=stay {
                assert!(hive.tick(false, false).is_empty(), "tick {tick} of a {stay}-tick stay");
            }
            let out = hive.tick(false, false);
            assert_eq!(out.len(), 1, "the {}th tick releases it", stay + 2);
            assert_eq!(out[0].honey_delivered, nectar);
            assert_eq!(out[0].occupant.ticks_in_hive, stay + 2);
        }
    }

    #[test]
    fn night_and_a_blocked_front_hold_a_due_bee_in_and_it_leaves_when_they_lift() {
        let mut hive = Beehive::default();
        hive.add(Occupant { entity_data: bee(false), ticks_in_hive: 700, min_ticks_in_hive: 600 }, None, false);
        assert!(hive.tick(true, false).is_empty(), "bees stay in at night");
        assert!(hive.tick(false, true).is_empty(), "a blocked front keeps them in");
        assert_eq!(hive.tick(false, false).len(), 1);
        assert!(hive.occupants().is_empty());
    }

    #[test]
    fn a_hive_holds_three_and_refuses_a_fourth() {
        let mut hive = Beehive::default();
        for _ in 0..3 {
            assert!(hive.add(Occupant::entering(bee(false)), None, false));
        }
        assert!(hive.is_full());
        assert!(!hive.add(Occupant::entering(bee(false)), None, false));
        assert_eq!(hive.occupants().len(), 3);
    }

    #[test]
    fn a_bee_reports_its_bloom_to_an_empty_hive_always_and_to_a_known_one_on_a_coin() {
        let mut hive = Beehive::default();
        let first = BlockPos::new(1, 2, 3);
        let second = BlockPos::new(4, 5, 6);
        hive.add(Occupant::entering(bee(false)), Some(first), false);
        assert_eq!(hive.flower(), Some(first));
        hive.add(Occupant::entering(bee(false)), Some(second), false);
        assert_eq!(hive.flower(), Some(first), "tails keeps the old bloom");
        hive.add(Occupant::entering(bee(false)), Some(second), true);
        assert_eq!(hive.flower(), Some(second), "heads replaces it");
    }

    #[test]
    fn honey_rises_by_one_or_two_and_stops_at_five() {
        // Hand arithmetic from the stated rule: a level-3 hive gains 1 on a
        // roll of 1..=99 and 2 on a roll of 0; a level-4 hive on a double roll
        // would overshoot to 6, so the extra is dropped; level 5 is unchanged.
        assert_eq!(honey_after_delivery(0, 50), 1);
        assert_eq!(honey_after_delivery(3, 0), 5);
        assert_eq!(honey_after_delivery(4, 0), 5);
        assert_eq!(honey_after_delivery(4, 1), 5);
        assert_eq!(honey_after_delivery(5, 0), 5);
    }

    #[test]
    fn breaking_a_hive_releases_everyone_with_no_honey() {
        let mut hive = Beehive::default();
        hive.add(Occupant::entering(bee(true)), None, false);
        hive.add(Occupant::entering(bee(false)), None, false);
        let out = hive.release_all();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|r| !r.honey_delivered));
        assert!(hive.occupants().is_empty());
    }

    #[test]
    fn the_saved_form_round_trips_and_matches_the_worldgen_shape() {
        let mut hive = Beehive::default();
        hive.add(Occupant { entity_data: bee(true), ticks_in_hive: 7, min_ticks_in_hive: 2400 }, Some(BlockPos::new(9, 64, -3)), false);
        let save = Nbt::Compound(hive.to_nbt_fields());
        assert_eq!(Beehive::from_nbt(&save), hive);
        // The generated nests the world generator writes carry exactly
        // `{entity_data:{id}, ticks_in_hive, min_ticks_in_hive}`.
        let generated = Nbt::Compound(vec![(
            "bees".to_owned(),
            Nbt::List {
                element_type: NbtTag::Compound,
                elements: vec![Nbt::Compound(vec![
                    ("entity_data".to_owned(), Nbt::Compound(vec![("id".to_owned(), Nbt::String("minecraft:bee".to_owned()))])),
                    ("ticks_in_hive".to_owned(), Nbt::Int(3)),
                    ("min_ticks_in_hive".to_owned(), Nbt::Int(600)),
                ])],
            },
        )]);
        let nest = Beehive::from_nbt(&generated);
        assert_eq!(nest.occupants().len(), 1);
        assert_eq!((nest.occupants()[0].ticks_in_hive, nest.occupants()[0].min_ticks_in_hive), (3, 600));
        assert!(!nest.occupants()[0].has_nectar());
    }

    #[test]
    fn a_malformed_occupant_is_skipped_and_extras_beyond_three_are_dropped() {
        let entry = |t: i32| {
            Nbt::Compound(vec![
                ("entity_data".to_owned(), bee(false)),
                ("ticks_in_hive".to_owned(), Nbt::Int(t)),
                ("min_ticks_in_hive".to_owned(), Nbt::Int(600)),
            ])
        };
        let save = Nbt::Compound(vec![(
            "bees".to_owned(),
            Nbt::List {
                element_type: NbtTag::Compound,
                elements: vec![entry(1), Nbt::Compound(vec![]), entry(2), entry(3), entry(4)],
            },
        )]);
        let hive = Beehive::from_nbt(&save);
        let ticks: Vec<i32> = hive.occupants().iter().map(|o| o.ticks_in_hive).collect();
        assert_eq!(ticks, vec![1, 2, 3]);
    }
}
