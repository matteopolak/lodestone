//! Particle event consumers and client-predicted environmental emitters.

use super::*;

/// Lowers a source-tagged state only where the built-in particle tables need a
/// generated-state index. A protocol-local value can overlap this build's
/// census, but its numeric range is not permission to render it as 26.2.
fn built_in_state_for_particles(
    state: BlockStateRef,
    effect: &str,
) -> Option<lodestone_data::block_states::StateId> {
    let BlockStateRef::Canonical(raw) = state else {
        tracing::debug!(
            target: "particles",
            raw = state.raw(),
            "protocol-local or custom block state for {effect}; not rendered by the built-in resolver"
        );
        return None;
    };
    let Some(state) = lodestone_data::block_states::StateId::new(raw) else {
        tracing::debug!(
            target: "particles",
            raw,
            "out-of-census canonical block state for {effect}; dropped"
        );
        return None;
    };
    Some(state)
}

impl Particles {
    /// Emit vanilla's block-destruction burst — vanilla's own client-level add-destroy-block-effect.
    ///
    /// The shape is passed in rather than queried because vanilla reads the
    /// block's *outline* shape, not its collision shape, and the two differ for
    /// exactly the blocks that matter: `short_grass` has an outline and no
    /// collision at all, so driving this from collision geometry would emit
    /// nothing when a player breaks grass.
    ///
    /// `tint` is an **extra** multiplier applied on top of the state's own
    /// particle tint, not a replacement for it — see
    /// [`state_tint_of`](Self::state_tint_of). Callers that have nothing special
    /// to say pass `[1.0; 3]`.
    pub fn destroy_block(&mut self, block: [i32; 3], state: BlockStateRef, tint: [f32; 3]) {
        let Some(state) = built_in_state_for_particles(state, "destroy debris") else {
            return;
        };
        let tint = self.state_tint_of(state, tint);
        emit::destroy_block_effect(
            &mut self.engine,
            (block[0], block[1], block[2]),
            state,
            tint,
            &[emit::FULL_CUBE],
        );
    }

    /// Emit the single fragment vanilla throws each time a mining hit lands on a
    /// face — vanilla's own client-level add-breaking-block-effect.
    ///
    /// `tint` is an extra multiplier on top of the state's own particle tint,
    /// exactly as in [`destroy_block`](Self::destroy_block): the two emitters
    /// both construct vanilla's own terrain particle, so they must tint identically or a
    /// block's mining flecks and its final burst come out different colours.
    pub fn breaking_block(
        &mut self,
        block: [i32; 3],
        state: BlockStateRef,
        tint: [f32; 3],
        face: emit::Face,
    ) {
        let Some(state) = built_in_state_for_particles(state, "mining debris") else {
            return;
        };
        let tint = self.state_tint_of(state, tint);
        emit::breaking_block_effect(
            &mut self.engine,
            (block[0], block[1], block[2]),
            state,
            tint,
            face,
            emit::FULL_CUBE,
        );
    }

    /// `extra` multiplied by `state`'s own particle tint — the same
    /// tint-source-multiply step of
    /// vanilla's own terrain-particle constructor.
    ///
    /// # Why this is folded in here rather than passed by the caller
    ///
    /// It was passed by the caller, as a hardcoded `[1.0; 3]` at both emit
    /// sites, and that is the bug this method exists to close: a *plausible*
    /// constant. The tinted blocks are precisely the ones whose atlas sprites
    /// are greyscale, so the missing multiply did not read as "slightly wrong
    /// colour" — it rendered grass, fern, leaf, sugar-cane and redstone debris
    /// **white**. Deriving it from the state id means a new emit site cannot
    /// reintroduce the constant by omission.
    ///
    /// `StateId` has already made the state-census range invariant true at the
    /// ingress. A partial tint table is still legitimate (the demo palette is
    /// empty), so its miss leaves the caller's multiplier alone.
    fn state_tint_of(
        &self,
        state: lodestone_data::block_states::StateId,
        extra: [f32; 3],
    ) -> [f32; 3] {
        let Some(t) = self.state_tint.get(state.raw() as usize) else {
            return extra;
        };
        [extra[0] * t[0], extra[1] * t[1], extra[2] * t[2]]
    }

    /// How many block states carry a non-white particle tint.
    ///
    /// This is an **anti-vacuity accessor**, not a game value: "no state's
    /// debris is the wrong colour" is satisfied by a table that resolved no
    /// tints at all, so a gate on particle tinting has to be able to prove the
    /// table is populated. Zero on the demo palette (correctly — it has no
    /// tinted blocks); in the thousands on a complete vanilla pack.
    #[must_use]
    pub fn tinted_state_count(&self) -> usize {
        self.state_tint
            .iter()
            .filter(|t| *t != &[1.0f32, 1.0, 1.0])
            .count()
    }

    /// Vanilla's own client-side particle-event handling — the general
    /// `LEVEL_PARTICLES` packet path, as opposed to the `LevelEvent` 2001
    /// shortcut [`Self::destroy_block`] covers. Spawns `count` particles of
    /// `kind` (the particle type's namespace-stripped path, e.g. `"flame"`)
    /// at `pos`.
    ///
    /// # `count == 0` is not "spawn nothing"
    ///
    /// Confirmed against the 26.2 client sources, vanilla's own
    /// particle-event packet handler:
    /// when `count == 0` vanilla spawns exactly **one** particle at the
    /// *exact* `pos` (no positional jitter), whose velocity is
    /// `maxSpeed * offset` per axis rather than drawn from noise:
    ///
    /// ```text
    /// if (count == 0) {
    ///     xa = maxSpeed * xDist; ya = maxSpeed * yDist; za = maxSpeed * zDist;
    ///     addParticle(particle, x, y, z, xa, ya, za);
    /// } else {
    ///     for (i in 0..count) {
    ///         xVarience = nextGaussian() * xDist; // ditto y, z
    ///         xa = nextGaussian() * maxSpeed;      // ditto y, z — NOT scaled by offset
    ///         addParticle(particle, x + xVarience, y + yVarience, z + zVarience, xa, ya, za);
    ///     }
    /// }
    /// ```
    ///
    /// So `offset` means two different things depending on `count`: a raw
    /// velocity direction when `count == 0`, and a per-axis jitter *bound*
    /// (multiplied by an independent gaussian draw) otherwise — and in the
    /// `count > 0` branch the velocity draws are unrelated to `offset`
    /// entirely, only to `max_speed`.
    ///
    /// Particle-burst randomness does not need to replay bit-exact against
    /// vanilla — nothing observes it across the wire, the same call
    /// `lodestone_particle`'s own `JavaRandom` docs make for the emitters
    /// below — so the gaussian draws here are an ordinary Box-Muller
    /// transform over the engine's existing RNG stream rather than a second
    /// `java.util.Random` reimplementation.
    ///
    /// Only particle types this shell has a dedicated emitter for are
    /// spawned; an unrecognised `kind` is logged and dropped. The shape of a
    /// burst lives in the per-type emitter ([`lodestone_particle::emit`]),
    /// and guessing at one here would just be a worse copy of it.
    pub fn spawn_particles(
        &mut self,
        kind: &str,
        pos: [f64; 3],
        offset: [f32; 3],
        max_speed: f32,
        count: i32,
        options: ParticleOptions,
    ) {
        if count == 0 {
            let vel = [
                f64::from(max_speed) * f64::from(offset[0]),
                f64::from(max_speed) * f64::from(offset[1]),
                f64::from(max_speed) * f64::from(offset[2]),
            ];
            self.spawn_one(kind, pos, vel, options);
            return;
        }
        for _ in 0..count {
            let jittered = [
                pos[0] + self.gaussian() * f64::from(offset[0]),
                pos[1] + self.gaussian() * f64::from(offset[1]),
                pos[2] + self.gaussian() * f64::from(offset[2]),
            ];
            let vel = [
                self.gaussian() * f64::from(max_speed),
                self.gaussian() * f64::from(max_speed),
                self.gaussian() * f64::from(max_speed),
            ];
            self.spawn_one(kind, jittered, vel, options);
        }
    }

    /// Dispatches one particle to the emitter matching `kind`. Mirrors
    /// vanilla's own add-particle per-type dispatch, narrowed to the sheet
    /// particles [`lodestone_particle::emit`] implements today.
    pub(crate) fn spawn_one(
        &mut self,
        kind: &str,
        pos: [f64; 3],
        vel: [f64; 3],
        options: ParticleOptions,
    ) {
        let [x, y, z] = pos;
        let [xa, ya, za] = vel;
        match kind {
            "flame" => emit::flame(&mut self.engine, x, y, z, xa, ya, za),
            "smoke" => emit::smoke(&mut self.engine, x, y, z, xa, ya, za, 1.0),
            // `LargeSmokeParticle extends SmokeParticle` with `scale = 2.5F`.
            "large_smoke" => emit::smoke(&mut self.engine, x, y, z, xa, ya, za, 2.5),
            "crit" => emit::crit(&mut self.engine, x, y, z, xa, ya, za),
            "splash" => emit::splash(&mut self.engine, x, y, z, xa, ya, za),
            "bubble" => emit::bubble(&mut self.engine, x, y, z, xa, ya, za),
            // The sweep-attack particle (that fix's split-out remainder — its own
            // issue now). `xa` doubles as the constructor's `size` parameter
            // here, per `AttackSweepParticle`'s own signature; see
            // `emit::sweep_attack`'s docs for why the one real vanilla call
            // site always sends `0.0` regardless of the swing direction.
            // The packet's own field is an f32; widened to f64 only for the
            // generic dispatch signature above, narrowed straight back here.
            "sweep_attack" => {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "narrowing back to the f32 the wire value started as"
                )]
                let size = xa as f32;
                emit::sweep_attack(&mut self.engine, x, y, z, size);
            }
            // Vanilla's own crit-particle family. All three share one constructor and
            // differ only in sheet plus a provider-level tweak; see
            // `emit::crit_particle`'s callers. `enchanted_hit` is the one this
            // client's own sprite table already knew about and nothing emitted
            // — `Sheet::EnchantedHit` has been stitched into the particle atlas
            // and unreachable since the sheet enum was written.
            "enchanted_hit" => emit::enchanted_hit(&mut self.engine, x, y, z, xa, ya, za),
            "damage_indicator" => emit::damage_indicator(&mut self.engine, x, y, z, xa, ya, za),
            // Vanilla's own spell-particle family, over the four sheets vanilla's own
            // `particles/*.json` assign it. `witch` (below) is the fifth
            // member; it draws its tint from the RNG rather than from a
            // provider constant, so it keeps its own emitter.
            //
            // `effect`/`instant_effect` carry vanilla's own spell-particle option (an RGB
            // word plus a velocity multiplier) and `entity_effect` a
            // `ColorParticleOption` (an ARGB word). Those payloads are the
            // *whole* of a potion particle's colour — the class has no palette
            // of its own — so a missing one draws a white mote, which looks
            // like a working particle and is why this went unnoticed. `v770`
            // decodes all three; the legacy families do not carry the payload
            // in this shape at all (1.12's `WORLD_PARTICLES` puts a mob-spell
            // tint in the offset words instead), so the fallback arms below
            // keep drawing white and **say so** rather than dropping the
            // particle, which would be a visible regression on those servers.
            "effect" => match options {
                ParticleOptions::Spell { color, power } => {
                    emit::spell_instant(
                        &mut self.engine,
                        x,
                        y,
                        z,
                        xa,
                        ya,
                        za,
                        Sheet::Effect,
                        color,
                        power,
                    );
                }
                _ => {
                    tracing::debug!(
                        target: "particles",
                        "effect particle with no spell-particle-option payload; \
                         drawing an untinted white mote"
                    );
                    emit::spell(&mut self.engine, x, y, z, xa, ya, za, Sheet::Effect, WHITE);
                }
            },
            "instant_effect" => match options {
                ParticleOptions::Spell { color, power } => {
                    emit::spell_instant(
                        &mut self.engine,
                        x,
                        y,
                        z,
                        xa,
                        ya,
                        za,
                        Sheet::Spell,
                        color,
                        power,
                    );
                }
                _ => {
                    tracing::debug!(
                        target: "particles",
                        "instant_effect particle with no spell-particle-option payload; \
                         drawing an untinted white mote"
                    );
                    emit::spell(&mut self.engine, x, y, z, xa, ya, za, Sheet::Spell, WHITE);
                }
            },
            "entity_effect" => match options {
                ParticleOptions::Color { color } => {
                    emit::spell_mob_effect(
                        &mut self.engine,
                        x,
                        y,
                        z,
                        xa,
                        ya,
                        za,
                        Sheet::Effect,
                        color,
                    );
                }
                _ => {
                    tracing::debug!(
                        target: "particles",
                        "entity_effect particle with no ColorParticleOption payload; \
                         drawing an untinted white mote"
                    );
                    emit::spell(&mut self.engine, x, y, z, xa, ya, za, Sheet::Effect, WHITE);
                }
            },
            "infested" => {
                emit::spell(&mut self.engine, x, y, z, xa, ya, za, Sheet::Infested, WHITE);
            }
            "raid_omen" => {
                emit::spell(&mut self.engine, x, y, z, xa, ya, za, Sheet::RaidOmen, WHITE);
            }
            "trial_omen" => {
                emit::spell(&mut self.engine, x, y, z, xa, ya, za, Sheet::TrialOmen, WHITE);
            }
            // Vanilla's own fly-towards-position particle's two argument-identical providers.
            // The wire's three velocity words are an **offset** for these, not a
            // velocity — see `emit::fly_towards_position`.
            "enchant" => {
                emit::fly_towards_position(&mut self.engine, x, y, z, xa, ya, za, Sheet::Enchant);
            }
            "nautilus" => {
                emit::fly_towards_position(&mut self.engine, x, y, z, xa, ya, za, Sheet::Nautilus);
            }
            "note" => emit::note(&mut self.engine, x, y, z, xa),
            "heart" => emit::heart(&mut self.engine, x, y, z),
            "angry_villager" => emit::angry_villager(&mut self.engine, x, y, z),
            "happy_villager" => emit::happy_villager(&mut self.engine, x, y, z, xa, ya, za),
            "witch" => emit::witch(&mut self.engine, x, y, z, xa, ya, za),
            "totem_of_undying" => emit::totem_of_undying(&mut self.engine, x, y, z, xa, ya, za),
            // `minecraft:explosion_emitter`/`minecraft:explosion`.
            // Correction the doc for these two carried until this pass: they
            // are **not** blocked on the shared `ParticleOptions` decoder
            // (`docs/particle-catalogue.md`'s "explosion_emitter"/"explosion"
            // section) — both are argument-less particle types with no encoded
            // fields, and
            // `decode_explode` already recognises their registry ids. What
            // was missing was exactly this arm plus the `Sheet`/`Behaviour`
            // pair in `lodestone_particle`, not a decoder.
            //
            // `explosion_emitter` (the seed vanilla's own explosion packet
            // actually names) ignores every
            // positional argument here: vanilla's own explosion-seed particle
            // constructor reads none. `explosion` reuses `xa` as the
            // constructor's `size` parameter, the same repurposing
            // `sweep_attack` above already does for its own `size`.
            "explosion_emitter" => emit::explosion_emitter(&mut self.engine, x, y, z),
            "explosion" => {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "narrowing back to the f32 the wire value started as"
                )]
                let size = xa as f32;
                emit::huge_explosion(&mut self.engine, x, y, z, size);
            }

            // -- Ambient and environmental types ----------------
            //
            // Every arm below is an argument-less `SimpleParticleType`, so the
            // three velocity words are exactly what the wire sent and nothing
            // needs the `ParticleOptions` decoder. Several *also* have a
            // client-predicted emitter — see `Sim::tick_ambient_particles` —
            // because vanilla spawns them from its own per-block animate-tick rather than
            // over the network; a type can legitimately have both.
            "soul" => emit::soul(&mut self.engine, x, y, z, xa, ya, za),
            "soul_fire_flame" => emit::soul_fire_flame(&mut self.engine, x, y, z, xa, ya, za),
            // `reverse_portal` shares vanilla's own portal-particle implementation and differs only in the
            // sign the *caller* gives the offset, which the wire already carries.
            "portal" | "reverse_portal" => emit::portal(&mut self.engine, x, y, z, xa, ya, za),
            "campfire_cosy_smoke" => {
                emit::campfire_smoke(&mut self.engine, x, y, z, xa, ya, za, false);
            }
            "campfire_signal_smoke" => {
                emit::campfire_smoke(&mut self.engine, x, y, z, xa, ya, za, true);
            }
            "end_rod" => emit::end_rod(&mut self.engine, x, y, z, xa, ya, za),
            // Vanilla's own glow-particle family, all five over `particle/glow`. These
            // two shared one approximation of its own firework spark particle
            // until this pass — a plausible-looking spark with the wrong
            // friction, the wrong lifetime, no tint and collision left on, and
            // `glow`'s own provider (a glow squid's two-population shimmer)
            // collapsed into `electric_spark`'s.
            "electric_spark" => emit::electric_spark(&mut self.engine, x, y, z, xa, ya, za),
            "glow" => emit::glow_squid(&mut self.engine, x, y, z, xa, ya, za),
            "scrape" => emit::scrape(&mut self.engine, x, y, z, xa, ya, za),
            "wax_on" => emit::wax_on(&mut self.engine, x, y, z, xa, ya, za),
            "wax_off" => emit::wax_off(&mut self.engine, x, y, z, xa, ya, za),
            // Vanilla's own flame-particle provider's other two registry types, and
            // its own small-flame provider. Each names its own sheet; the shared
            // provider decides nothing.
            "copper_fire_flame" => {
                emit::copper_fire_flame(&mut self.engine, x, y, z, xa, ya, za);
            }
            "small_flame" => emit::small_flame(&mut self.engine, x, y, z, xa, ya, za),
            "sculk_soul" => emit::sculk_soul(&mut self.engine, x, y, z, xa, ya, za),
            "sculk_charge_pop" => emit::sculk_charge_pop(&mut self.engine, x, y, z, xa, ya, za),
            // Vanilla's own player-cloud-particle's two providers. An area-effect cloud's
            // puff and a panda's sneeze.
            "cloud" => emit::cloud(&mut self.engine, x, y, z, xa, ya, za),
            "sneeze" => emit::sneeze(&mut self.engine, x, y, z, xa, ya, za),
            // Vanilla's own lava-particle reads none of the three velocity words: its
            // constructor damps them to 0.8 and then overwrites `yd` outright,
            // so every pop launches upward whatever the packet said. It is also
            // the only particle here that spawns a *different* type as it
            // lives — see `Behaviour::Lava`'s trailing-smoke roll.
            "lava" => emit::lava(&mut self.engine, x, y, z),
            "squid_ink" => emit::squid_ink(&mut self.engine, x, y, z, xa, ya, za),
            "glow_squid_ink" => emit::glow_squid_ink(&mut self.engine, x, y, z, xa, ya, za),
            // Vanilla's own firework spark particle via its own spark provider -- the plain
            // wire particle a `LEVEL_PARTICLES` packet can name directly, not the
            // rocket-explosion burst its own starter/no-render particle spawns
            // client-side (never sent over the wire at all). See
            // `docs/particle-catalogue.md`'s "Correction" entry for why this was
            // never blocked on the `ParticleOptions` decoder the way it first
            // looked: vanilla's own firework particle type is a `SimpleParticleType`.
            "firework" => emit::firework(&mut self.engine, x, y, z, xa, ya, za),
            // Vanilla's own dragon-breath particle — a dragon's breath attack and, far more
            // commonly, every lingering potion cloud. Its `PowerParticleOption`
            // is a velocity multiplier and nothing else; the purple is drawn
            // per particle inside the emitter, so a missing payload costs
            // motion rather than colour and the fallback is power 1.0
            // (`PowerParticleOption`'s own data-codec default).
            "dragon_breath" => match options {
                ParticleOptions::Power { power } => {
                    emit::dragon_breath(&mut self.engine, x, y, z, xa, ya, za, power);
                }
                _ => {
                    tracing::debug!(
                        target: "particles",
                        "dragon_breath particle with no PowerParticleOption payload; \
                         drawing at unit power"
                    );
                    emit::dragon_breath(&mut self.engine, x, y, z, xa, ya, za, 1.0);
                }
            },
            // `SculkChargeParticle` has its own emitter rather than sharing
            // `animated_ambient` with the three below: its roll is a wire
            // field, its lifetime a per-particle draw, and its provider
            // installs the packet's velocity words verbatim.
            "sculk_charge" => match options {
                ParticleOptions::SculkCharge { roll } => {
                    emit::sculk_charge(&mut self.engine, x, y, z, xa, ya, za, roll);
                }
                _ => {
                    tracing::debug!(
                        target: "particles",
                        "sculk_charge particle with no SculkChargeParticleOptions payload; \
                         drawing at zero roll"
                    );
                    emit::sculk_charge(&mut self.engine, x, y, z, xa, ya, za, 0.0);
                }
            },
            // Sheet, scale and lifetime are what separate these three; the tick
            // shape is identical. Lifetimes are each class's own constructor.
            "gust" => {
                emit::animated_ambient(&mut self.engine, x, y, z, 0.0, 0.0, 0.0, Sheet::Gust, 3.0, 12)
            }
            // `Sheet::SmallGust`, not `Sheet::Gust`: vanilla's own small-gust
            // provider
            // shares the class but `small_gust.json` names `small_gust_0`…`_6`,
            // seven frames of its own. Pointed at `Gust` this sampled the wrong
            // texture and indexed a twelve-frame sequence it does not have.
            "small_gust" => emit::animated_ambient(
                &mut self.engine, x, y, z, 0.0, 0.0, 0.0, Sheet::SmallGust, 1.0, 12,
            ),
            "sonic_boom" => emit::animated_ambient(
                &mut self.engine, x, y, z, 0.0, 0.0, 0.0, Sheet::SonicBoom, 3.0, 16,
            ),
            // The drip family: seventeen registry types over one class, one
            // `(kind, phase)` table in `emit::drip`, and a chain that continues
            // **inside** the particle's own tick — a hanging drip spawns the
            // falling one when it lets go, and that spawns the splash or the
            // landing phase where it hits.
            //
            // Only the five below existed before, all as unchained one-shots
            // with a hardcoded 64-tick lifetime, so a cave ceiling grew drips
            // that hung for the wrong length of time and then blinked out
            // without ever falling.
            //
            // These take no velocity from the packet by design: vanilla's
            // providers all use vanilla's own drip-particle's zero-velocity constructor, and
            // the only velocity a drip ever has is the one its hanging phase
            // hands to its falling phase.
            "dripping_water" => self.drip(DripKind::Water, DripPhase::Hang, pos),
            "falling_water" => self.drip(DripKind::Water, DripPhase::Fall, pos),
            "dripping_lava" => self.drip(DripKind::Lava, DripPhase::Hang, pos),
            "falling_lava" => self.drip(DripKind::Lava, DripPhase::Fall, pos),
            "landing_lava" => self.drip(DripKind::Lava, DripPhase::Land, pos),
            "dripping_honey" => self.drip(DripKind::Honey, DripPhase::Hang, pos),
            "falling_honey" => self.drip(DripKind::Honey, DripPhase::Fall, pos),
            "landing_honey" => self.drip(DripKind::Honey, DripPhase::Land, pos),
            "falling_nectar" => self.drip(DripKind::Nectar, DripPhase::Fall, pos),
            "dripping_obsidian_tear" => self.drip(DripKind::ObsidianTear, DripPhase::Hang, pos),
            "falling_obsidian_tear" => self.drip(DripKind::ObsidianTear, DripPhase::Fall, pos),
            "landing_obsidian_tear" => self.drip(DripKind::ObsidianTear, DripPhase::Land, pos),
            "dripping_dripstone_water" => {
                self.drip(DripKind::DripstoneWater, DripPhase::Hang, pos);
            }
            "falling_dripstone_water" => {
                self.drip(DripKind::DripstoneWater, DripPhase::Fall, pos);
            }
            "dripping_dripstone_lava" => self.drip(DripKind::DripstoneLava, DripPhase::Hang, pos),
            "falling_dripstone_lava" => self.drip(DripKind::DripstoneLava, DripPhase::Fall, pos),
            "falling_spore_blossom" => self.drip(DripKind::SporeBlossom, DripPhase::Fall, pos),
            // `spore_blossom_air` used to sit in this drip block, and it is
            // not a drip particle at all — vanilla's own suspended-particle.
            // SporeBlossomAirProvider`, which shares `drip_fall`'s *texture*
            // with `falling_spore_blossom` and nothing else. It hangs rather
            // than falling, and its lifetime is a flat 500..=1000 ticks against
            // the drip's own draw, so as a drip it vanished far too fast.
            "spore_blossom_air" => emit::spore_blossom_air(&mut self.engine, x, y, z),

            // -- Vanilla's own suspended-particle biome drift ----------------
            //
            // Four types over one class. Each supplies its own velocity inside
            // the emitter (vanilla's providers draw it, rather than taking it
            // from the packet), so the wire's three velocity words are
            // deliberately unused here — that is the class's shape, not a
            // dropped field.
            "underwater" => emit::underwater(&mut self.engine, x, y, z),
            "crimson_spore" => emit::crimson_spore(&mut self.engine, x, y, z),
            "warped_spore" => emit::warped_spore(&mut self.engine, x, y, z),

            // -- The `SuspendedTownParticle` ambient specks ----------------
            "mycelium" => emit::mycelium(&mut self.engine, x, y, z, xa, ya, za),
            "composter" => emit::composter(&mut self.engine, x, y, z, xa, ya, za),
            "egg_crack" => emit::egg_crack(&mut self.engine, x, y, z, xa, ya, za),
            "dolphin" => emit::dolphin(&mut self.engine, x, y, z, xa, ya, za),

            // -- `BaseAshSmokeParticle`'s other three subclasses ----------------
            //
            // `ash` and `white_ash` take no velocity from the packet either;
            // `white_ash` draws its own and `ash` has none.
            "ash" => emit::ash(&mut self.engine, x, y, z),
            "white_ash" => emit::white_ash(&mut self.engine, x, y, z),
            "white_smoke" => emit::white_smoke(&mut self.engine, x, y, z, xa, ya, za),

            // -- `ExplodeParticle` ----------------
            //
            // `poof` is the mob-death, breeding and spawner puff — among the
            // most frequently spawned particles in the game, and until this arm
            // existed every one of them hit the catch-all below.
            "poof" => emit::poof(&mut self.engine, x, y, z, xa, ya, za),
            "spit" => emit::spit(&mut self.engine, x, y, z, xa, ya, za),
            // The two `ParticleOptions`-carrying types this shell decodes a
            // payload for today (`decode_particle_options` in the v770
            // adapter). `kind` and `options` both come from the same
            // `LEVEL_PARTICLES` packet by construction -- `net.rs`'s
            // `ClientEvent::Particles` arm carries both straight through to
            // `NetUpdate::Particles`, and `net_apply.rs`'s arm hands both to
            // this call unmodified -- so the two agreeing is the production
            // case; the `_` arm below is only reachable from a caller
            // (a test, or a future non-network producer) that passes a
            // mismatched or default `options` on purpose.
            "dust" => match options {
                ParticleOptions::Dust { color, scale } => {
                    emit::dust(&mut self.engine, x, y, z, xa, ya, za, color, scale);
                }
                _ => tracing::debug!(
                    target: "particles",
                    "dust particle with no DustParticleOptions payload; dropped"
                ),
            },
            "dust_color_transition" => match options {
                ParticleOptions::DustColorTransition { from_color, to_color, scale } => {
                    emit::dust_color_transition(
                        &mut self.engine,
                        x,
                        y,
                        z,
                        xa,
                        ya,
                        za,
                        from_color,
                        to_color,
                        scale,
                    );
                }
                _ => tracing::debug!(
                    target: "particles",
                    "dust_color_transition particle with no DustColorTransitionOptions \
                     payload; dropped"
                ),
            },

            // -- The `BlockParticleOption` family ------------------
            //
            // One wire payload, five providers. The payload is shared and the
            // *behaviour* is not: three build vanilla's own terrain particle (differing in
            // speed and lifetime), one a physics-free marker quad, one a
            // sheet-textured mote wearing the block's colour rather than its
            // texture. Every arm goes through `block_state_payload`, which is
            // where the `isAir`/`moving_piston` refusal lives — vanilla's
            // vanilla's own create-terrain-particle returns `null` for those and a fragment of
            // air is a fragment of nothing.
            "block" => {
                if let Some(state) = self.block_state_payload(kind, options) {
                    let tint = self.state_tint_of(state, [1.0; 3]);
                    emit::block_fragment(&mut self.engine, pos, vel, state, tint);
                }
            }
            "block_crumble" => {
                if let Some(state) = self.block_state_payload(kind, options) {
                    let tint = self.state_tint_of(state, [1.0; 3]);
                    emit::block_crumble(&mut self.engine, pos, vel, state, tint);
                }
            }
            "dust_pillar" => {
                if let Some(state) = self.block_state_payload(kind, options) {
                    let tint = self.state_tint_of(state, [1.0; 3]);
                    emit::dust_pillar(&mut self.engine, pos, vel, state, tint);
                }
            }
            // No tint: vanilla's own block-marker constructor never touches `rCol`, so a
            // marker over grass is the grass sprite at full brightness, not the
            // `0.6`-grey-times-biome-tint a fragment of it would be.
            "block_marker" => {
                if let Some(state) = self.block_state_payload(kind, options) {
                    emit::block_marker(&mut self.engine, pos, state);
                }
            }
            // The tint is the *whole* identity here — the sprite is a generic
            // grey mote — so this one reads `state_tint_of` for a purpose the
            // other four only decorate with.
            "falling_dust" => {
                if let Some(state) = self.block_state_payload(kind, options) {
                    let tint = self.state_tint_of(state, [1.0; 3]);
                    emit::falling_dust(&mut self.engine, pos, tint);
                }
            }

            // -- The water column and weather family ---------------
            //
            // `rain` is the splash a raindrop makes where it *lands*, not the
            // falling streaks: those are `lodestone_render::weather`'s textured
            // columns, which never become particles at all. Wiring `rain` here
            // does not duplicate them.
            "rain" => emit::rain(&mut self.engine, x, y, z),
            "snowflake" => emit::snowflake(&mut self.engine, x, y, z, xa, ya, za),
            "bubble_column_up" => {
                emit::bubble_column_up(&mut self.engine, x, y, z, xa, ya, za);
            }
            // `WaterCurrentDownParticle`'s provider ignores the packet's
            // velocity entirely — the sink speed is a constant and the drift is
            // the spiral. Passing the wire's words would give every magma
            // column an initial kick vanilla does not have.
            "current_down" => emit::current_down(&mut self.engine, x, y, z),
            "bubble_pop" => emit::bubble_pop(&mut self.engine, x, y, z, xa, ya, za),
            // The bobber's ring. Its producer is the fishing bobber entity,
            // which already draws; this is the water it disturbs.
            "fishing" => emit::fishing(&mut self.engine, x, y, z, xa, ya, za),
            "dust_plume" => emit::dust_plume(&mut self.engine, x, y, z, xa, ya, za),

            // -- `FallingLeavesParticle` ---------------------------
            //
            // One class, three registry types, and the providers differ in five
            // constants at once — see `emit::LeafParams`, which carries them as
            // a set so a transposed pair cannot hide. The two untinted variants
            // take no colour; `tinted_leaves` carries a `ColorParticleOption`.
            "cherry_leaves" => {
                emit::falling_leaves(&mut self.engine, x, y, z, emit::LeafParams::cherry(), None);
            }
            "pale_oak_leaves" => {
                emit::falling_leaves(&mut self.engine, x, y, z, emit::LeafParams::pale_oak(), None);
            }
            "tinted_leaves" => match options {
                ParticleOptions::Color { color } => {
                    emit::falling_leaves(
                        &mut self.engine,
                        x,
                        y,
                        z,
                        emit::LeafParams::tinted(),
                        Some([color[0], color[1], color[2]]),
                    );
                }
                _ => tracing::debug!(
                    target: "particles",
                    "tinted_leaves particle with no ColorParticleOption payload; dropped"
                ),
            },

            "firefly" => emit::firefly(&mut self.engine, x, y, z, ya),
            // Vanilla's own firework flash provider reads all four ARGB components:
            // the alpha byte is a real field here, not padding, and dropping it
            // makes every firework flash fully opaque.
            "flash" => match options {
                ParticleOptions::Color { color } => emit::flash(&mut self.engine, x, y, z, color),
                _ => tracing::debug!(
                    target: "particles",
                    "flash particle with no ColorParticleOption payload; dropped"
                ),
            },

            // -- `BreakingItemParticle`'s hardcoded-item providers --
            //
            // Three `SimpleParticleType`s with **no wire payload**: each
            // provider names its own item and calls the four-argument
            // constructor. The item ids are resolved here rather than baked
            // into `lodestone-particle`, which knows nothing about the item
            // registry, and a missing id drops the particle rather than
            // drawing a wrong sprite.
            "item_slime" => self.item_burst(pos, "minecraft:slime_ball"),
            "item_cobweb" => self.item_burst(pos, "minecraft:cobweb"),
            "item_snowball" => self.item_burst(pos, "minecraft:snowball"),

            // The geyser eruption seed. Its wire payload's own water-blocks
            // field this client's own decoder does not yet surface (there is
            // no `ParticleOptions` variant for it), so every geyser draws at
            // the minimum valid payload value (`1`) rather than dropping the
            // particle — the same "log and use a harmless default" shape the
            // `dragon_breath`/`sculk_charge` arms above use for their own
            // missing payloads. `geyser_base`/`geyser_poof`/`geyser_plume`
            // are vanilla's own eruption particle's own three children, drawn
            // through the same emitters here so a direct `/particle` of one
            // of those three (never how vanilla itself spawns them) still
            // draws something.
            "geyser" => {
                tracing::debug!(
                    target: "particles",
                    "geyser particle with no decoded water-blocks payload; drawing at \
                     water_blocks = 1"
                );
                emit::geyser(&mut self.engine, x, y, z, xa, ya, za, 1);
            }
            "geyser_base" => {
                emit::geyser_base_or_poof(&mut self.engine, x, y, z, xa, ya, za, 1, 1.5, Sheet::GeyserBase);
            }
            "geyser_poof" => {
                emit::geyser_base_or_poof(&mut self.engine, x, y, z, xa, ya, za, 1, 2.0, Sheet::GeyserPoof);
            }
            "geyser_plume" => emit::geyser_plume(&mut self.engine, x, y, z, xa, ya, za, 1),

            // The potent-sulfur block's noxious-gas family: the puff itself,
            // fixed scale 3.0, and the non-rendering seed that throws puffs
            // around itself every two ticks for its own 20-tick life.
            "noxious_gas" => emit::noxious_gas(&mut self.engine, x, y, z, xa, ya, za),
            "noxious_gas_cloud" => emit::noxious_gas_cloud(&mut self.engine, x, y, z),
            // The potent-sulfur spring's rising bubble and the debris a
            // broken sulfur cube throws. The bubble reads no `ya`: vanilla's
            // own provider drops it too.
            "sulfur_bubbles" => emit::sulfur_bubbles(&mut self.engine, x, y, z, xa, za),
            "sulfur_cube_goo" => emit::sulfur_cube_goo(&mut self.engine, x, y, z),
            // The trial spawner's and the vault's own detection runes — one
            // class, two sheets, no wire payload.
            "trial_spawner_detection" => emit::trial_spawner_detection(
                &mut self.engine, x, y, z, xa, ya, za, Sheet::TrialSpawnerDetection,
            ),
            "trial_spawner_detection_ominous" => emit::trial_spawner_detection(
                &mut self.engine, x, y, z, xa, ya, za, Sheet::TrialSpawnerDetectionOminous,
            ),
            "vault_connection" => {
                emit::vault_connection(&mut self.engine, x, y, z, xa, ya, za);
            }
            "ominous_spawning" => {
                emit::ominous_spawning(&mut self.engine, x, y, z, xa, ya, za);
            }
            // The wind-charge/gust-emitter seeds — vanilla's own two provider
            // registrations' own constants, never wire-driven.
            "gust_emitter_large" => emit::gust_emitter(&mut self.engine, x, y, z, 3.0, 7, 0),
            "gust_emitter_small" => emit::gust_emitter(&mut self.engine, x, y, z, 1.0, 3, 2),
            "pause_mob_growth" => {
                emit::simple_vertical(&mut self.engine, x, y, z, xa, ya, za, false);
            }
            "reset_mob_growth" => {
                emit::simple_vertical(&mut self.engine, x, y, z, xa, ya, za, true);
            }
            // The sculk shrieker's shockwave. Its wire payload carries a
            // delay this client's own decoder does not yet surface (there is
            // no `ParticleOptions` variant for it), so every shriek draws
            // with delay `0` (immediate) rather than dropping the particle.
            "shriek" => emit::shriek(&mut self.engine, x, y, z, 0),

            other => tracing::debug!(
                target: "particles",
                "no emitter wired for particle type {other:?}; dropped"
            ),
        }
    }

    /// The block state a `BlockParticleOption` particle should wear, or `None`
    /// if this one must not spawn at all.
    ///
    /// Two refusals, and they are different in kind. A missing payload is a
    /// *caller* fault — production cannot produce one, since the adapter decodes
    /// the state alongside the type and hands both through together, so this can
    /// only be reached from a test or a future non-network producer, and it is
    /// logged rather than asserted for the same reason every other payload arm
    /// here logs.
    ///
    /// The second is vanilla's own: its own create-terrain-particle returns `null` for
    /// air and for `moving_piston`, and vanilla's own falling-dust-particle
    /// provider refuses
    /// an invisible-render-shape state. Air is the one that matters — a
    /// `LevelEvent`-adjacent producer that reads a block *after* it has been
    /// removed sends the air state, and without this test the client spends a
    /// full burst of particles on a sprite that resolves to nothing and lands in
    /// [`ParticleFrame::unresolved`], where it reads as a broken atlas rather
    /// than as a refusal vanilla also makes.
    ///
    /// `moving_piston` is included because it is the second half of the same
    /// vanilla condition and costs one string compare; the invisible-render-shape
    /// clause is **not** ported, because this client has no per-state render-shape
    /// table and the states it would catch (barriers, structure voids, light
    /// blocks) have no particle sprite either, so they are already refused one
    /// layer down.
    fn block_state_payload(
        &self,
        kind: &str,
        options: ParticleOptions,
    ) -> Option<lodestone_data::block_states::StateId> {
        let ParticleOptions::BlockState { state } = options else {
            tracing::debug!(
                target: "particles",
                "{kind} particle with no BlockParticleOption payload; dropped"
            );
            return None;
        };
        let BlockStateRef::Canonical(raw) = state else {
            tracing::debug!(
                target: "particles",
                raw = state.raw(),
                "{kind} particle with a protocol-local or custom \
                 BlockParticleOption state; not rendered by the built-in resolver"
            );
            return None;
        };
        let Some(state) = lodestone_data::block_states::StateId::new(raw) else {
            tracing::debug!(
                target: "particles",
                "{kind} particle with an out-of-census canonical BlockParticleOption state; dropped"
            );
            return None;
        };
        if matches!(
            state.block(),
            lodestone_data::block::Block::Air
                | lodestone_data::block::Block::CaveAir
                | lodestone_data::block::Block::VoidAir
                | lodestone_data::block::Block::MovingPiston
        ) {
            return None;
        }
        Some(state)
    }

    /// One `BreakingItemParticle` from the four-argument constructor, for the
    /// three registry types whose provider hardcodes an item.
    ///
    /// A named helper for the same reason [`Self::drip`] is one: the whole of
    /// what it adds is the registry lookup and the zero velocity, and spelling
    /// those three times is three chances to reach for [`emit::item_particle`]
    /// — the *seven*-argument sibling, which damps the jitter to a tenth and
    /// would leave these crumbs motionless.
    fn item_burst(&mut self, pos: [f64; 3], item: &str) {
        let Some(item) = Item::from_name(item) else {
            tracing::debug!(
                target: "particles",
                "no registry id for {item:?}; item particle dropped"
            );
            return;
        };
        let particle = emit::item_burst_particle(pos[0], pos[1], pos[2], item, self.engine.rng());
        self.engine.add(particle);
    }

    /// One drip of the hang → fall → land chain, at the packet's position.
    ///
    /// A named helper rather than seventeen `emit::drip(&mut self.engine, kind,
    /// phase, pos, [0.0; 3])` calls: the zero velocity is the *whole* of what
    /// this adds, and spelling it seventeen times is seventeen chances to pass
    /// the packet's velocity words instead. Vanilla's providers all use
    /// its own drip-particle's zero-velocity constructor; the only velocity a drip ever
    /// carries is the one its hanging phase hands on when it lets go.
    fn drip(&mut self, kind: DripKind, phase: DripPhase, pos: [f64; 3]) {
        emit::drip(&mut self.engine, kind, phase, pos, [0.0; 3]);
    }

    /// Vanilla's own client-level particle-level calculation folded together with
    /// its own add-particle "particle level not minimal" test — `true` to spawn.
    ///
    /// Transcribed rather than approximated, because the two halves are not
    /// separable: the fold is what makes `DECREASED` a *probability* rather
    /// than a second fixed budget.
    ///
    /// The level starts at the option's own setting; if always-show is set and
    /// the level is minimal, a one-in-ten roll promotes it to decreased; then,
    /// independently, a one-in-three roll on a decreased level demotes it back
    /// to minimal. The particle spawns whenever the resulting level is not
    /// minimal.
    ///
    /// So `All` always spawns, `Decreased` spawns two times in three, and
    /// `Minimal` spawns only via the always-show reprieve — `(1/10) x (2/3)`,
    /// i.e. one time in fifteen.
    ///
    /// `always_show` reaches here from the wire: `LevelParticles::always_show`
    /// on a 26.2 connection, threaded through `ClientEvent::Particles` and
    /// `NetUpdate::Particles` to `net_apply.rs`'s arm. It is `false` on every
    /// legacy family because the field does not exist on their particle
    /// packets, which is the same value vanilla's own particle-spawn overload
    /// passes, not an unported one.
    ///
    /// Note the reprieve is a *probability*, not an exemption: an always-show
    /// particle on `Minimal` still fails fourteen times in fifteen. A gate on
    /// this therefore has to count over many draws or pin the RNG; a single
    /// call proves nothing either way.
    ///
    /// Draws from the particle engine's own `JavaRandom`, which is
    /// `java.util.Random`-compatible, so `next_i32_bound` is vanilla's
    /// `nextInt` exactly. Not the same *stream* as vanilla's own per-level
    /// random source,
    /// which does not matter: nothing observes particle randomness across the
    /// wire.
    pub fn particle_level_permits(
        &mut self,
        level: crate::config::ParticleLevel,
        always_show: bool,
    ) -> bool {
        use crate::config::ParticleLevel;
        let mut level = level;
        if always_show && level == ParticleLevel::Minimal && self.engine.rng().next_i32_bound(10) == 0
        {
            level = ParticleLevel::Decreased;
        }
        if level == ParticleLevel::Decreased && self.engine.rng().next_i32_bound(3) == 0 {
            level = ParticleLevel::Minimal;
        }
        level != ParticleLevel::Minimal
    }

    /// One standard-normal draw (Box-Muller), for the positional/velocity
    /// jitter [`Self::spawn_particles`] needs. See that method's docs for why
    /// this does not need to match `java.util.Random.nextGaussian()`
    /// bit-for-bit.
    fn gaussian(&mut self) -> f64 {
        let rng = self.engine.rng();
        let u1 = rng.next_f64().max(1e-12);
        let u2 = rng.next_f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// Test-only seam: installs a `(Sheet, frame) -> UV rect` table directly,
    /// bypassing `ParticleAtlas`/jar I/O — mirrors the fixture this module's
    /// own tests use (see `sheet_particle_resolves_with_an_atlas`), exposed so
    /// `crate::sim`'s tests can assert a live `NetUpdate::Particles` resolves
    /// without needing the real vanilla jar.
    #[cfg(test)]
    pub(crate) fn install_test_sheet_uv(&mut self, table: HashMap<(Sheet, u16), [f32; 4]>) {
        self.sheet_uv = Arc::new(table);
    }

}

impl Particles {
    /// Tick the main plume for every loaded lit campfire block entity.
    ///
    /// Minecraft 26.2 runs this from its own campfire-block-entity particle
    /// tick, once
    /// per client simulation tick. Each source has an independent 11% chance
    /// to emit a burst of two or three particles. Its own campfire-block
    /// animate-tick
    /// owns only the occasional lava fleck and crackle sound, so putting this
    /// in [`Self::ambient_tick`] both misses campfires outside that random scan
    /// and gives sampled ones the wrong probability.
    pub fn campfire_block_entity_tick(&mut self, sources: &[([i32; 3], bool)]) {
        for &(block, signal) in sources {
            if self.engine.rng().next_f32() >= 0.11 {
                continue;
            }
            let count = self.engine.rng().next_i32_bound(2) + 2;
            for _ in 0..count {
                let (x, y, z) = {
                    let rng = self.engine.rng();
                    let x_offset = rng.next_f64() / 3.0;
                    let x_sign = if rng.next_bool() { 1.0 } else { -1.0 };
                    let y_offset = rng.next_f64() + rng.next_f64();
                    let z_offset = rng.next_f64() / 3.0;
                    let z_sign = if rng.next_bool() { 1.0 } else { -1.0 };
                    (
                        f64::from(block[0]) + 0.5 + x_offset * x_sign,
                        f64::from(block[1]) + y_offset,
                        f64::from(block[2]) + 0.5 + z_offset * z_sign,
                    )
                };
                emit::campfire_smoke(
                    &mut self.engine,
                    x,
                    y,
                    z,
                    0.0,
                    0.07,
                    0.0,
                    signal,
                );
            }
        }
    }

    /// Emit this tick's **client-predicted** ambient particles — vanilla's own
    /// per-block animate-tick, which is not on the wire at all.
    ///
    /// # Why this cannot be a server-event consumer
    ///
    /// A torch's flame, a nether portal's shimmer and an end rod's sparkle are
    /// spawned by vanilla's own client-level animate-tick walking random nearby positions and
    /// calling each block's own `animateTick`. **No packet carries them**, so a
    /// client that only consumed `LEVEL_PARTICLES` would show a torch-lit room
    /// with no flames however complete its dispatch table was. That is the shape
    /// of the gap this closes, and it is why several of the types below *also*
    /// have a `spawn_one` arm: the same type can arrive both ways.
    ///
    /// # The probe, and why it is a closure
    ///
    /// `probe` answers "what block state is at this position" and is injected
    /// rather than taken as a world reference, exactly as `ShellAmbience::tick`
    /// injects its light probe: the two callers hold *different* view types (a
    /// live 3×3 column snapshot, or the offline demo world) and neither is
    /// nameable here. A probe returning `0` (air) for an unloaded position is
    /// correct — nothing should be emitted there.
    ///
    /// Sampling is uniform over a box around `eye`, so the cost is
    /// [`AMBIENT_SAMPLES`] probes per tick regardless of how much is nearby.
    pub fn ambient_tick(&mut self, eye: [f64; 3], probe: &mut impl FnMut([i32; 3]) -> u32) {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "block coordinates; the eye is always within i32 range"
        )]
        let centre = [
            eye[0].floor() as i32,
            eye[1].floor() as i32,
            eye[2].floor() as i32,
        ];
        for _ in 0..AMBIENT_SAMPLES {
            let span = AMBIENT_RANGE * 2 + 1;
            let rng = self.engine.rng();
            let offset = [
                rng.next_i32_bound(span) - AMBIENT_RANGE,
                rng.next_i32_bound(span) - AMBIENT_RANGE,
                rng.next_i32_bound(span) - AMBIENT_RANGE,
            ];
            let block = [
                centre[0] + offset[0],
                centre[1] + offset[1],
                centre[2] + offset[2],
            ];
            let Some(state) = lodestone_data::block_states::StateId::new(probe(block)) else {
                continue;
            };
            if state.block() == lodestone_data::block::Block::Air {
                continue;
            }
            self.animate_block(block, state);
        }
    }

    /// One block's `animateTick`, for the handful of blocks a survival player
    /// actually notices. Silent for everything else.
    fn animate_block(
        &mut self,
        block: [i32; 3],
        state: lodestone_data::block_states::StateId,
    ) {
        let block_kind = state.block();
        let props = state.properties();
        let prop = |key: &str| props.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        let [bx, by, bz] = [
            f64::from(block[0]),
            f64::from(block[1]),
            f64::from(block[2]),
        ];
        match block_kind {
            // Vanilla's own torch-block animate-tick: one flame and one smoke at the flame's
            // own position, which for a wall torch is offset *away* from the wall
            // it hangs on. Using the block centre for both puts the flame inside
            // the wall.
            lodestone_data::block::Block::Torch
            | lodestone_data::block::Block::SoulTorch
            | lodestone_data::block::Block::WallTorch
            | lodestone_data::block::Block::SoulWallTorch => {
                let (dx, dz, dy) = match prop("facing") {
                    Some("north") => (0.0, 0.27, 0.22),
                    Some("south") => (0.0, -0.27, 0.22),
                    Some("west") => (0.27, 0.0, 0.22),
                    Some("east") => (-0.27, 0.0, 0.22),
                    // A standing torch: centred, flame at the tip.
                    _ => (0.0, 0.0, 0.0),
                };
                let (x, y, z) = (bx + 0.5 + dx, by + 0.7 + dy, bz + 0.5 + dz);
                emit::smoke(&mut self.engine, x, y, z, 0.0, 0.0, 0.0, 1.0);
                if matches!(
                    block_kind,
                    lodestone_data::block::Block::SoulTorch
                        | lodestone_data::block::Block::SoulWallTorch
                ) {
                    emit::soul_fire_flame(&mut self.engine, x, y, z, 0.0, 0.0, 0.0);
                } else {
                    emit::flame(&mut self.engine, x, y, z, 0.0, 0.0, 0.0);
                }
            }
            // Vanilla's own nether-portal-block animate-tick: four motes per tick at random
            // points inside the block, drifting on a signed offset — which for
            // vanilla's own portal particle is the *amplitude* it converges from, not a speed.
            lodestone_data::block::Block::NetherPortal
            | lodestone_data::block::Block::EndGateway => {
                for _ in 0..4 {
                    let rng = self.engine.rng();
                    let (rx, ry, rz) = (
                        f64::from(rng.next_f32()),
                        f64::from(rng.next_f32()),
                        f64::from(rng.next_f32()),
                    );
                    let sign = |r: &mut lodestone_particle::rng::JavaRandom| {
                        if r.next_bool() { 1.0 } else { -1.0 }
                    };
                    let rng = self.engine.rng();
                    let (sx, sz) = (sign(rng), sign(rng));
                    emit::portal(
                        &mut self.engine,
                        bx + rx,
                        by + ry,
                        bz + rz,
                        sx * 0.25,
                        (ry - 0.5) * 0.25,
                        sz * 0.25,
                    );
                }
            }
            // Vanilla's own end-rod-block animate-tick: one sparkle just off the rod's tip,
            // along whatever axis it points.
            lodestone_data::block::Block::EndRod => {
                let (dx, dy, dz) = match prop("facing") {
                    Some("up") => (0.0, 0.4, 0.0),
                    Some("down") => (0.0, -0.4, 0.0),
                    Some("north") => (0.0, 0.0, -0.4),
                    Some("south") => (0.0, 0.0, 0.4),
                    Some("west") => (-0.4, 0.0, 0.0),
                    _ => (0.4, 0.0, 0.0),
                };
                emit::end_rod(
                    &mut self.engine,
                    bx + 0.5 + dx,
                    by + 0.5 + dy,
                    bz + 0.5 + dz,
                    0.0,
                    0.0,
                    0.0,
                );
            }
            _ => {}
        }
    }
}
