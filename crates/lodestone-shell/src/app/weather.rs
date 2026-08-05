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

/// The world knowledge [`lodestone_render::extract_columns`] needs, resolved from
/// **one** light sample per frame rather than one per column.
///
/// # Why one sample, and what it costs
///
/// Vanilla samples a heightmap, a biome and a lightmap **per column** (441 of each
/// at the default radius, `WeatherEffectRenderer.java:72-88`), reading a level it
/// owns directly. This client reaches the world through
/// [`crate::net::entity_light_at`], which takes the client's world lock **per
/// call**; 441 locks per frame at 60 fps is not a trade worth making for a first
/// landing, so the probe is built from a single sample at the camera and answers
/// every column from it.
///
/// The three divergences, in order of how visible they are:
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
    /// Every biome's declared climate (issue #25), published once at `Login`
    /// by [`crate::net::forward`]'s `BiomeClimates` arm. `None` off a live
    /// connection.
    pub(super) biome_climates: Option<crate::net::SharedBiomeClimates>,
}

impl ShellWeatherProbe {
    /// Resolve `(x, y, z)`'s standing biome and translate its declared
    /// climate to a [`lodestone_render::Precipitation`] via vanilla's own
    /// `getPrecipitationAt` (`Biome.java:104-108`), height-adjusted the same
    /// way `Biome.getHeightAdjustedTemperature` is (`Biome.java:110-121`).
    ///
    /// `None` at any hop — world not loaded, section elided (all-air), the
    /// climate table still empty, or the biome's own `temperature`/
    /// `has_precipitation` unresolved — is exactly "the server has not told
    /// us yet", the same open set `Sim::biome_sky_color`'s doc already
    /// enumerates for the sky-colour lookup this mirrors. The caller decides
    /// the fallback.
    fn biome_precipitation(&self, x: i32, y: i32, z: i32) -> Option<lodestone_render::Precipitation> {
        let handle = self.handle.as_ref()?;
        let dims = handle.world_dimensions()?;
        let chunk = lodestone_client::ChunkPos {
            x: x.div_euclid(16),
            z: z.div_euclid(16),
        };
        let base_si = dims.min_y.div_euclid(16);
        let si = y.div_euclid(16) - base_si;
        if si < 0 || (si as usize) >= dims.section_count() {
            return None;
        }
        let section = handle.section_at(chunk, si as usize)?;
        let biome = section.biome_at_block(
            x.rem_euclid(16) as usize,
            y.rem_euclid(16) as usize,
            z.rem_euclid(16) as usize,
        );
        let climate = self
            .biome_climates
            .as_ref()?
            .get(usize::try_from(biome).ok()?)?;
        // `worldgen::SEA_LEVEL` (63), not a second `63` constant — see the
        // #25 report's own note to grep for one before adding a duplicate.
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
}

impl lodestone_render::WeatherProbe for ShellWeatherProbe {
    fn column_top(&self, _x: i32, _z: i32) -> Option<i32> {
        None
    }

    fn precipitation(&self, x: i32, y: i32, z: i32) -> lodestone_render::Precipitation {
        if !self.sky_visible {
            return lodestone_render::Precipitation::None;
        }
        // Issue #25: the biome climate lane now reaches the client
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
