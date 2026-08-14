//! Weather and time-of-day plumbing for the render crate's rain/snow columns.
//!
//! Split out of `app.rs`; see that module's own header for the layout.

use super::*;

/// Extrapolates the server's `time_of_day` continuously between the ~1/sec
/// `SET_TIME` packets that are its only source (`WorldTime` is a flat
/// snapshot — see the doc at both [`WindowApp::connect_to`] call sites for
/// why the raw value alone made the sky's cloud scroll visibly step once a
/// second). `advance` is meant to be polled once per frame from a
/// [`RenderState::set_time_of_day_source`](crate::gpu::RenderState::set_time_of_day_source)
/// closure: on a still-current tick it adds elapsed wall-clock time at the
/// standard 20 ticks/sec, and on a new tick from the network it re-anchors —
/// the same local-prediction-then-correct shape vanilla's own client-side
/// day-time uses. `Mutex`, not `Cell`, only because the closure trait bound is
/// `Fn` (shared refs) rather than `FnMut`.
pub(super) struct ContinuousTimeOfDay(std::sync::Mutex<Option<(i64, Instant)>>);

impl ContinuousTimeOfDay {
    pub(super) fn new() -> Self {
        Self(std::sync::Mutex::new(None))
    }

    pub(super) fn advance(&self, server_tick: i64) -> i64 {
        let mut anchor = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        match *anchor {
            Some((tick, at)) if tick == server_tick => {
                tick + (now.duration_since(at).as_secs_f64() * 20.0) as i64
            }
            _ => {
                *anchor = Some((server_tick, now));
                server_tick
            }
        }
    }
}

/// How long a lightning flash is held, in wall-clock time.
///
/// [`lodestone_render::LIGHTNING_FLASH_TICKS`] is 5 game ticks, which is 250 ms at
/// the standard 20 ticks/sec. Timed off the wall clock rather than the tick clock
/// because the two consumers are a per-*frame* render source and a per-frame fog
/// composition, neither of which has a tick edge to hang a countdown on — the same
/// reason [`ContinuousTimeOfDay`] extrapolates rather than stepping.
const LIGHTNING_FLASH_HOLD: Duration = Duration::from_millis(
    (lodestone_render::LIGHTNING_FLASH_TICKS as u64) * 1000 / 20,
);

/// Resolves the net thread's raw weather scalars into a
/// [`lodestone_render::WeatherState`], and times the lightning flash.
///
/// One of these is shared (`Arc`) between the `set_sky_darken_source` closure and
/// `redraw`'s per-frame fog/column composition, so both halves of "weather" read
/// the **same** state on the same frame. Two independent reads of the cell would
/// be almost identical and occasionally not, and a lightmap disagreeing with the
/// sky it is lit by is exactly the class of bug that reads as a shader problem.
///
/// `Mutex` for the same reason [`ContinuousTimeOfDay`] uses one: the render
/// source's trait bound is `Fn`, not `FnMut`.
#[derive(Debug)]
pub(super) struct WeatherTracker {
    cell: crate::net::SharedWeather,
    flash: std::sync::Mutex<(u64, Option<Instant>)>,
}

impl WeatherTracker {
    pub(super) fn new(cell: crate::net::SharedWeather) -> Self {
        Self {
            cell,
            flash: std::sync::Mutex::new((0, None)),
        }
    }

    /// This frame's weather.
    ///
    /// The two levels are handed to `WeatherState` **raw** and it does the
    /// clamping and the `thunder × rain` composition — see
    /// `lodestone_render::weather`'s module doc for why composing them here
    /// instead would black out a clear sky on join.
    pub(super) fn state(&self) -> lodestone_render::WeatherState {
        let snapshot = self.cell.snapshot();
        let mut state = lodestone_render::WeatherState::clear();
        state.apply_rain_level(snapshot.rain_level);
        state.apply_thunder_level(snapshot.thunder_level);

        let now = Instant::now();
        let mut flash = self
            .flash
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if flash.0 != snapshot.lightning_seq {
            // A new bolt (or several — the seq can jump by more than one between
            // frames; one flash either way, which is also what vanilla shows).
            *flash = (snapshot.lightning_seq, Some(now));
        }
        if let Some(started) = flash.1 {
            if now.duration_since(started) < LIGHTNING_FLASH_HOLD {
                state.flash();
            } else {
                // Cleared rather than left set, so a long session does not keep
                // re-reading a stale `Instant` every frame.
                flash.1 = None;
            }
        }
        state
    }
}

/// The world knowledge [`lodestone_render::extract_columns`] needs: **one** light
/// sample per frame, and a per-column biome answered through a per-frame memo
/// ([`ProbeMemo`]) so the square costs a handful of world locks rather than ~1300.
///
/// # What each answer costs, and what this doc used to claim
///
/// Vanilla samples a heightmap, a biome and a lightmap **per column** (441 of each
/// at the default radius, `WeatherEffectRenderer.java`), reading a level it
/// owns directly. This client reaches the world through
/// [`crate::net::entity_light_at`] and [`lodestone_client::ClientHandle`], each of
/// which takes the client's world lock **per call**.
///
/// * **Light: one sample per frame**, at the eye, reused for every column — the
///   third divergence below.
/// * **Biome: one `section_at` per distinct chunk column**, through
///   [`ProbeMemo`]. The 21×21 *block* square covers **4, 6 or 9 chunk columns**
///   (21 consecutive blocks straddle two chunk boundaries whenever the camera sits
///   near one, so it is not always 4), plus one `world_dimensions` and one climate
///   lookup per distinct biome holder id.
///
/// **This doc used to say the probe answered every column from that one light
/// sample, and it was wrong.** It was written before that fix added the real
/// per-column biome lookup, which put three world locks back on the per-column
/// path — 441 × 3 acquisitions per rainy frame, contended against the chunk
/// streaming writer — while both this comment and `redraw.rs`'s repeated the cheap
/// design. A comment describing a design the code does not implement is worse than
/// none, because it stops the next reader looking (`DESIGN.md` §12.114). The memo
/// is what makes the claim true again.
///
/// The three divergences from vanilla, in order of how visible they are:
///
/// * **No per-column terrain height.** `column_top` is `None`, so every column
///   spans `camera_y ± radius` instead of stopping at the ground. Invisible — the
///   pass is depth-tested, so sub-surface fragments are occluded — but it costs
///   vertices that vanilla would not draw. Closing it needs a `column_height`
///   accessor on `ClientHandle`; the heightmaps are already decoded into
///   `lodestone_world::LoadedChunk::heightmaps` and nothing reads them yet.
/// * **Sky visibility is the camera's, not the column's.** In a cave the camera's
///   own sky light is 0 and the whole pass draws nothing, which is right; standing
///   at a cave *mouth* it draws rain across the cavern, which is wrong. Vanilla's
///   per-column `canSeeSky` is what fixes it, and it needs the same heightmap
///   accessor.
/// * **One light level for the whole square.** Rain seen through a shaded gully is
///   as bright as rain in the open. Barely visible in practice: rain is drawn
///   outdoors, where sky light is uniform.
///
/// Rain-versus-snow is **not** in that list, because it is not an approximation
/// here — it is missing data. See
/// [`lodestone_render::WeatherProbe::precipitation`].
pub(super) struct ShellWeatherProbe {
    /// The already-resolved lightmap term at the camera, weather included.
    pub(super) light: f32,
    /// Whether any sky light reaches the camera. `false` draws no precipitation at
    /// all, which is the cave case.
    pub(super) sky_visible: bool,
    /// The client-owned world, resolved once per frame the same way `packed`
    /// above is (a plain `Arc` clone out of the `SharedHandle`'s `OnceLock`,
    /// not a lock held across the frame) — needed for the per-column biome
    /// lookup [`Self::biome_precipitation`] does. `None` before login.
    pub(super) handle: Option<Arc<lodestone_client::ClientHandle>>,
    /// Every biome's declared climate, published once at `Login`
    /// by [`crate::net::forward`]'s `BiomeClimates` arm. `None` off a live
    /// connection.
    pub(super) biome_climates: Option<crate::net::SharedBiomeClimates>,
    /// This frame's cache of the three world reads above.
    ///
    /// **A probe must not outlive the frame it was built for.** The memo is
    /// what makes the column loop cheap and it is also the only thing in this
    /// struct that can go stale: a section cached here is a snapshot, so a probe
    /// reused across frames would keep serving pre-`BLOCK_UPDATE` biomes. Every
    /// producer builds one per frame (`redraw.rs`) — keep it that way.
    pub(super) memo: ProbeMemo,
}

/// One frame's cache of the three world reads
/// [`ShellWeatherProbe::biome_precipitation`] needs, so each is taken **once per
/// distinct key** instead of once per column.
///
/// # Why it exists
///
/// `precipitation` is called once per column — 441 times a frame at
/// [`lodestone_render::DEFAULT_WEATHER_RADIUS`] — and each call used to take three
/// locks: [`lodestone_client::ClientHandle::world_dimensions`], `section_at` (plus
/// an `Arc<ChunkSection>` clone) and the [`crate::net::BiomeClimateCell`] mutex.
/// None of the three varies per *column*: the dimensions are per session, the
/// section is per **chunk column and section index**, and the climate is per biome
/// holder id. The keys are what changes, and there are a handful of them.
///
/// # How to change it, and the gotchas
///
/// * **`Mutex`, not `RefCell`**, and not for contention: the probe is held across
///   an `.await` in `app::tests`' `live` gate, so `Send` is a real bound. Every
///   acquisition here is uncontended and none of them touches the world lock.
/// * **[`Self::with_section`] reads *through* the cached `Arc` rather than handing
///   one out**, so the per-column path clones nothing. A `-> Option<Arc<_>>`
///   accessor would put 441 atomic increments back.
/// * **Linear scan, deliberately.** The section table holds at most 9 entries and
///   the climate table one per biome in view; a `HashMap` would hash more than the
///   scan compares.
/// * The fetch closure runs **while the memo lock is held**, so it may take the
///   world lock but nothing it calls may re-enter the memo.
#[derive(Debug, Default)]
pub(super) struct ProbeMemo {
    /// `None` = not yet fetched; `Some(None)` = fetched and absent.
    dims: std::sync::Mutex<Option<Option<lodestone_client::WorldDimensions>>>,
    /// Keyed `(chunk x, chunk z, section index)`.
    sections: std::sync::Mutex<Vec<((i32, i32, usize), Option<Arc<lodestone_client::ChunkSection>>)>>,
    /// Keyed by biome holder id.
    climates: std::sync::Mutex<Vec<(u32, Option<crate::net::BiomeClimateEntry>)>>,
}

impl ProbeMemo {
    /// The world dimensions, fetched at most once per probe.
    fn dimensions(
        &self,
        fetch: impl FnOnce() -> Option<lodestone_client::WorldDimensions>,
    ) -> Option<lodestone_client::WorldDimensions> {
        let mut slot = self
            .dims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot.get_or_insert_with(fetch)
    }

    /// Read something out of the section at `key`, fetching it at most once per
    /// key. `None` when the section is absent (unloaded column, or elided
    /// all-air) — the same answer `section_at` gives, cached.
    fn with_section<R>(
        &self,
        key: (i32, i32, usize),
        fetch: impl FnOnce() -> Option<Arc<lodestone_client::ChunkSection>>,
        read: impl FnOnce(&lodestone_client::ChunkSection) -> R,
    ) -> Option<R> {
        let mut table = self
            .sections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = match table.iter().position(|(k, _)| *k == key) {
            Some(index) => index,
            None => {
                table.push((key, fetch()));
                table.len() - 1
            }
        };
        table[index].1.as_deref().map(read)
    }

    /// The climate of biome holder `id`, fetched at most once per id.
    fn climate(
        &self,
        id: u32,
        fetch: impl FnOnce() -> Option<crate::net::BiomeClimateEntry>,
    ) -> Option<crate::net::BiomeClimateEntry> {
        let mut table = self
            .climates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match table.iter().find(|(k, _)| *k == id) {
            Some((_, entry)) => *entry,
            None => {
                let entry = fetch();
                table.push((id, entry));
                entry
            }
        }
    }

    /// The whole per-column resolve: `(x, y, z)`'s standing biome translated to
    /// a [`lodestone_render::Precipitation`] via vanilla's own
    /// `getPrecipitationAt` (`Biome.java`), height-adjusted the same way
    /// `Biome.getHeightAdjustedTemperature` is (`Biome.java`), with all
    /// three world reads memoised.
    ///
    /// The three reads arrive as closures because that is what lets one type own
    /// the whole resolve *and* what lets the gate count them — see the type doc.
    /// `None` at any hop — world not loaded, section elided (all-air), the
    /// climate table still empty, or the biome's own `temperature`/
    /// `has_precipitation` unresolved — is exactly "the server has not told us
    /// yet"; the caller decides the fallback.
    fn precipitation_at(
        &self,
        x: i32,
        y: i32,
        z: i32,
        dimensions: impl FnOnce() -> Option<lodestone_client::WorldDimensions>,
        section: impl FnOnce(
            lodestone_client::ChunkPos,
            usize,
        ) -> Option<Arc<lodestone_client::ChunkSection>>,
        climate: impl FnOnce(u32) -> Option<crate::net::BiomeClimateEntry>,
    ) -> Option<lodestone_render::Precipitation> {
        let dims = self.dimensions(dimensions)?;
        let (chunk, si) = section_key(&dims, x, y, z)?;
        let biome = self.with_section(
            (chunk.x, chunk.z, si),
            || section(chunk, si),
            |section| {
                section.biome_at_block(
                    x.rem_euclid(16) as usize,
                    y.rem_euclid(16) as usize,
                    z.rem_euclid(16) as usize,
                )
            },
        )?;
        let climate = self.climate(biome, || climate(biome))?;
        // `worldgen::SEA_LEVEL` (63), not a second `63` constant — see the
        // That fix report's own note to grep for one before adding a duplicate.
        let temperature = lodestone_render::weather::height_adjusted_temperature(
            climate.temperature?,
            y,
            crate::worldgen::SEA_LEVEL,
        );
        Some(lodestone_render::weather::precipitation_for_temperature(
            climate.has_precipitation?,
            temperature,
        ))
    }

    /// How many `section_at` fetches this memo has performed — i.e. how many
    /// world locks the column loop cost. The gate's counter; see
    /// `one_section_fetch_per_chunk_column_not_one_per_column`.
    #[cfg(test)]
    fn section_fetches(&self) -> usize {
        self.sections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

/// The `(chunk, section index)` a block position resolves to, or `None` when it
/// is outside the world's vertical range.
///
/// Shared by [`ShellWeatherProbe::biome_precipitation`] and by the memo gate, so
/// the gate measures the key derivation production uses rather than a restatement
/// of it — a per-*block* key would fetch 441 sections and still look correct.
pub(super) fn section_key(
    dims: &lodestone_client::WorldDimensions,
    x: i32,
    y: i32,
    z: i32,
) -> Option<(lodestone_client::ChunkPos, usize)> {
    let base_si = dims.min_y.div_euclid(16);
    let si = y.div_euclid(16) - base_si;
    if si < 0 || (si as usize) >= dims.section_count() {
        return None;
    }
    Some((
        lodestone_client::ChunkPos {
            x: x.div_euclid(16),
            z: z.div_euclid(16),
        },
        si as usize,
    ))
}

impl ShellWeatherProbe {
    /// Resolve `(x, y, z)`'s standing biome and translate its declared
    /// climate to a [`lodestone_render::Precipitation`] via vanilla's own
    /// `getPrecipitationAt` (`Biome.java`), height-adjusted the same
    /// way `Biome.getHeightAdjustedTemperature` is (`Biome.java`).
    ///
    /// `None` at any hop — world not loaded, section elided (all-air), the
    /// climate table still empty, or the biome's own `temperature`/
    /// `has_precipitation` unresolved — is exactly "the server has not told
    /// us yet", the same open set `Sim::biome_sky_color`'s doc already
    /// enumerates for the sky-colour lookup this mirrors. The caller decides
    /// the fallback.
    fn biome_precipitation(&self, x: i32, y: i32, z: i32) -> Option<lodestone_render::Precipitation> {
        let handle = self.handle.as_ref()?;
        let climates = self.biome_climates.as_ref()?;
        // The three world reads are passed in as closures rather than called
        // here, so [`ProbeMemo::precipitation_at`] owns the whole resolve and
        // every read is memoised. It is also the only way the gate can count
        // them: there is no hermetic `ClientHandle`.
        self.memo.precipitation_at(
            x,
            y,
            z,
            || handle.world_dimensions(),
            |chunk, si| handle.section_at(chunk, si),
            |biome| climates.get(usize::try_from(biome).ok()?),
        )
    }
}

impl lodestone_render::WeatherProbe for ShellWeatherProbe {
    fn column_top(&self, _x: i32, _z: i32) -> Option<i32> {
        None
    }

    fn precipitation(&self, x: i32, y: i32, z: i32) -> lodestone_render::Precipitation {
        if !self.sky_visible {
            return lodestone_render::Precipitation::None;
        }
        // The biome climate lane now reaches the client
        // (`ClientEvent::BiomeClimates`, decoded and folded via
        // `net::BiomeClimateCell`), so this resolves a real per-column
        // answer instead of hardcoding `Rain`. Every unresolved hop still
        // falls back to `Rain` — matching `sky_visible`'s own "absent data
        // reads as open sky" rule: an unlit fallback here would make the
        // first rainy frame after joining silently show nothing.
        self.biome_precipitation(x, y, z)
            .unwrap_or(lodestone_render::Precipitation::Rain)
    }

    fn light(&self, _x: i32, _y: i32, _z: i32) -> f32 {
        self.light
    }
}

/// This frame's precipitation quads and the rain/snow split point, ready for
/// [`crate::gpu::RenderState::prepare_weather`].
///
/// A free function, not a `WindowApp` method: `redraw` holds a live `&mut` borrow
/// of `self.render` across the call site, so any `&self` method would be a second
/// borrow of the same struct.
///
/// The two returned values travel together on purpose. `extract_columns` sorts
/// rain-first so the pass can bind two textures over one buffer, and the count is
/// only meaningful against *that* ordering — a count taken from a differently
/// sorted list textures snow as rain with no error anywhere.
pub(super) fn weather_columns_for_frame(
    weather: &lodestone_render::WeatherState,
    camera: &lodestone_render::Camera,
    tick: u64,
    probe: &dyn lodestone_render::WeatherProbe,
) -> (Vec<lodestone_render::WeatherInstance>, usize) {
    let camera_pos = [
        f64::from(camera.position.x),
        f64::from(camera.position.y),
        f64::from(camera.position.z),
    ];
    // The animation phase is driven by the **tick** clock, not by frame time.
    // `rain_column`'s scroll is `-(ticks + offset + partial) / 32 * speed`, so
    // feeding it a frame counter makes the fall speed frame-rate dependent — the
    // defect `entities.rs`'s
    // `limb_swing_tracks_per_tick_travel_not_the_interpolation_gap` records for the
    // walk cycle. `partial_ticks` is 0.0 rather than the real sub-tick alpha: at
    // 3-4 texture tiles per tick the sub-tick smoothing is below one texel, and
    // `Sim` exposes no partial tick to this layer.
    let columns = lodestone_render::extract_columns(
        weather,
        lodestone_render::DEFAULT_WEATHER_RADIUS,
        tick as i64,
        0.0,
        camera_pos,
        probe,
    );
    let rain = lodestone_render::rain_count(&columns);
    let offsets = lodestone_render::column_offset_table();
    let instances = columns
        .iter()
        .map(|c| {
            lodestone_render::column_instance(
                c,
                camera_pos,
                &offsets,
                lodestone_render::DEFAULT_WEATHER_RADIUS,
                weather.rain_level(),
            )
        })
        .collect();
    (instances, rain)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use lodestone_render::{DEFAULT_WEATHER_RADIUS, Precipitation, WeatherProbe, WeatherState};
    use lodestone_world::PaletteKind;

    use super::*;

    /// Columns in the square [`lodestone_render::extract_columns`] walks, from the
    /// same radius `weather_columns_for_frame` passes rather than a literal.
    const COLUMNS: usize =
        ((2 * DEFAULT_WEATHER_RADIUS + 1) * (2 * DEFAULT_WEATHER_RADIUS + 1)) as usize;

    /// A rainy world 384 blocks tall from `y = -64`, i.e. the real overworld
    /// geometry: the section index a `y = 64` sample lands in is
    /// `64.div_euclid(16) - (-64).div_euclid(16)` = `4 + 4` = `8`, comfortably
    /// inside the 24 sections, so no column is rejected by the range check and
    /// every column reaches the memo.
    const DIMS: lodestone_client::WorldDimensions = lodestone_client::WorldDimensions {
        min_y: -64,
        height: 384,
    };

    /// Biome holder id whose climate rains (temperature 0.8 — the value the real
    /// registry entry carries, per `v770`'s own
    /// `biome_sky_colours_resolve_by_holder_id` fixture).
    const WARM_BIOME: u32 = 1;
    /// Biome holder id whose climate snows (temperature 0.0, below vanilla's
    /// `warmEnoughToRain` 0.15 at `Biome.java`).
    const COLD_BIOME: u32 = 7;

    /// Drives the **production** resolve ([`ProbeMemo::precipitation_at`]) with
    /// the three world reads replaced by counting closures.
    ///
    /// The seam is at the `ClientHandle` boundary and nowhere else, because there
    /// is no hermetic way to build a `ClientHandle` — it only comes out of
    /// `ClientBuilder::connect`, so the alternative to this is a live oracle. The
    /// key derivation ([`section_key`]), the memo and the climate maths are all
    /// the real thing; only `ShellWeatherProbe`'s `sky_visible` early-out and its
    /// two `?`s on absent wiring are outside the gate.
    ///
    /// `ClientHandle::section_at`/`ClientHandle::sections_at` are documented as taking the
    /// internal world lock **exactly once**, which is what makes
    /// `section_fetches` a count of world locks rather than of function calls.
    struct CountingWorld {
        section: Arc<lodestone_client::ChunkSection>,
        dim_fetches: AtomicUsize,
        section_fetches: AtomicUsize,
        climate_fetches: AtomicUsize,
        memo: ProbeMemo,
    }

    impl CountingWorld {
        /// One section whose 4×4×4 biome cells alternate between a warm and a
        /// cold biome along x, so the square spans **two** biomes: a memo that
        /// cached one climate for every id would answer `Rain` everywhere and
        /// still count correctly.
        fn new() -> Self {
            let mut section = lodestone_client::ChunkSection::new(
                PaletteKind::block_states_with_direct_bits(20),
                PaletteKind::biomes(),
                0,
                WARM_BIOME,
            );
            for y in 0..4 {
                for z in 0..4 {
                    for x in 0..4 {
                        let biome = if x % 2 == 0 { WARM_BIOME } else { COLD_BIOME };
                        section.set_biome(x, y, z, biome);
                    }
                }
            }
            Self {
                section: Arc::new(section),
                dim_fetches: AtomicUsize::new(0),
                section_fetches: AtomicUsize::new(0),
                climate_fetches: AtomicUsize::new(0),
                memo: ProbeMemo::default(),
            }
        }
    }

    impl WeatherProbe for CountingWorld {
        fn column_top(&self, _x: i32, _z: i32) -> Option<i32> {
            None
        }

        fn precipitation(&self, x: i32, y: i32, z: i32) -> Precipitation {
            self.memo
                .precipitation_at(
                    x,
                    y,
                    z,
                    || {
                        self.dim_fetches.fetch_add(1, Ordering::Relaxed);
                        Some(DIMS)
                    },
                    |_chunk, _si| {
                        self.section_fetches.fetch_add(1, Ordering::Relaxed);
                        Some(Arc::clone(&self.section))
                    },
                    |biome| {
                        self.climate_fetches.fetch_add(1, Ordering::Relaxed);
                        Some(crate::net::BiomeClimateEntry {
                            temperature: Some(if biome == COLD_BIOME { 0.0 } else { 0.8 }),
                            downfall: Some(0.4),
                            has_precipitation: Some(true),
                        })
                    },
                )
                // The same fallback `ShellWeatherProbe::precipitation` applies.
                .unwrap_or(Precipitation::Rain)
        }

        fn light(&self, _x: i32, _y: i32, _z: i32) -> f32 {
            1.0
        }
    }

    fn rainy() -> WeatherState {
        let mut weather = WeatherState::clear();
        weather.apply_rain_level(1.0);
        weather
    }

    fn camera(x: f32, y: f32, z: f32) -> lodestone_render::Camera {
        lodestone_render::Camera {
            position: glam::Vec3::new(x, y, z),
            ..Default::default()
        }
    }

    /// The counter this whole change exists to move: world locks per rainy frame.
    ///
    /// Both competing hypotheses are stated as numbers rather than as a
    /// direction. **441** is the pre-memo implementation, which fetched per
    /// column; **1** is a hoist that lost the per-column key and would answer
    /// every column from the camera's own chunk. The right answer is the number
    /// of distinct chunk columns the block square covers, worked out from the
    /// block range by hand:
    ///
    /// * camera x = 0 → blocks `-10..=10` → `div_euclid(16)` ∈ `{-1, 0}` = **2**
    ///   columns per axis, so **4** sections;
    /// * camera x = 24 → blocks `14..=34` → `{0, 1, 2}` = **3** per axis, so
    ///   **9**. 21 consecutive blocks straddle two chunk boundaries whenever the
    ///   camera sits near one, which is why "at most 4" is wrong.
    #[test]
    fn one_section_fetch_per_chunk_column_not_one_per_column() {
        for (cam, expected) in [(0.5_f32, 4_usize), (24.5, 9)] {
            let world = CountingWorld::new();
            let (instances, rain) = weather_columns_for_frame(
                &rainy(),
                &camera(cam, 64.5, cam),
                0,
                &world,
            );
            assert_eq!(
                instances.len(),
                COLUMNS,
                "every column must reach the probe, or the count below measures \
                 a shorter square than production walks"
            );
            assert!(
                rain > 0 && rain < COLUMNS,
                "the square must span both biomes, or one climate lookup would \
                 answer every column and the count below would be satisfied by a \
                 memo that ignores its key: {rain} rain of {COLUMNS}"
            );
            let fetched = world.section_fetches.load(Ordering::Relaxed);
            assert_eq!(
                fetched, expected,
                "camera {cam}: expected {expected} `section_at` calls (one per \
                 chunk column the square covers); {COLUMNS} is the pre-memo \
                 per-column implementation and 1 is a hoist that lost the key"
            );
            assert_eq!(
                world.memo.section_fetches(),
                fetched,
                "the memo's own table length and the closure's counter must agree"
            );
            assert_eq!(
                world.dim_fetches.load(Ordering::Relaxed),
                1,
                "`world_dimensions` is per session, so it is fetched once per frame"
            );
            assert_eq!(
                world.climate_fetches.load(Ordering::Relaxed),
                2,
                "one climate lookup per distinct biome holder id in view, and the \
                 fixture puts exactly two there"
            );
        }
    }

    /// The correctness half: memoising must not collapse two biomes into one
    /// answer. Without this, `one_section_fetch_per_chunk_column_not_one_per_column`
    /// is satisfied by a memo that caches the first climate it sees for every id.
    #[test]
    fn the_memo_still_answers_snow_and_rain_per_column() {
        let world = CountingWorld::new();
        let mut rain = 0;
        let mut snow = 0;
        for x in -10..=10 {
            match world.precipitation(x, 64, 0) {
                Precipitation::Rain => rain += 1,
                Precipitation::Snow => snow += 1,
                Precipitation::None => {}
            }
        }
        // Predicted, not merely non-zero. Biome cells are 4 blocks wide and
        // alternate warm/cold on the cell index, so a block rains when
        // `((x.rem_euclid(16)) >> 2) % 2 == 0`. Over `-10..=10` that is
        // `-8..=-5`, `0..=3` and `8..=10` — 4 + 4 + 3 = **11** rain and 10 snow.
        // A memo that served the first climate to every id would read 21 and 0.
        assert_eq!(
            (rain, snow),
            (11, 10),
            "expected 11 rain / 10 snow from the alternating 4-block biome cells; \
             (21, 0) is the collapsed-climate hypothesis"
        );
        // ...and the two biomes really did cost two climate lookups, not 21.
        assert_eq!(world.climate_fetches.load(Ordering::Relaxed), 2);
    }

    /// The world-species control: a clear frame cannot exercise any of this, so a
    /// gate built on `WeatherState::clear()` would pass whatever the memo did.
    #[test]
    fn a_clear_frame_touches_the_world_not_at_all() {
        let world = CountingWorld::new();
        let (instances, rain) = weather_columns_for_frame(
            &WeatherState::clear(),
            &camera(0.5, 64.5, 0.5),
            0,
            &world,
        );
        assert!(instances.is_empty() && rain == 0);
        assert_eq!(world.section_fetches.load(Ordering::Relaxed), 0);
        assert_eq!(world.dim_fetches.load(Ordering::Relaxed), 0);
    }
}
