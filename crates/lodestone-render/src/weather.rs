//! Weather: rain/snow columns, the darkening rain and thunder apply to the sky
//! and the lightmap, the lightning flash, and the rain ambience cadence.
//!
//! This module is the **pure** half — vanilla's constants and arithmetic, with no
//! GPU and no world access. The pass that turns [`WeatherInstance`]s into pixels
//! is [`crate::weather_pipeline`]; the wire → state plumbing is
//! `lodestone_shell::net`'s `forward` arm for `ClientEvent::WeatherChanged`.
//!
//! # Where every number here comes from
//!
//! Read out of `.cache/mc/26.2/{client-src,src}` at the paths named per item.
//! The load-bearing ones:
//!
//! | quantity | value | source |
//! |---|---|---|
//! | rain/snow table size | 32 × 32, centred at 16 | `WeatherEffectRenderer.java:43-44` |
//! | rain column speed | `3.0 + rand` | `WeatherEffectRenderer.java:168` |
//! | rain texture scroll | `-(ticks + offset + partial) / 32 * speed`, wrapped at 32 | `:169-170` |
//! | snow V drift | `-((ticks & 511) + partial) / 512` | `:181` |
//! | per-column seed | `x*x*3121 + x*45238971 ^ z*z*418711 + z*13761` | `:80` |
//! | rain max alpha | `1.0` | `:133` |
//! | snow max alpha | `0.8` | `:134` |
//! | distance alpha | `lerp(min(d²/r², 1), max_alpha, 0.5) * intensity` | `:201` |
//! | V from height | `y * 0.25 + v_offset` | `:214-215` |
//! | snow light boost | `(l * 3 + 15) / 4` per half | `:182` |
//! | rain→snow threshold | temperature `>= 0.15` is rain | `Biome.java:175-176` |
//! | `getThunderLevel` | `thunder * rain` | `Level.java:918-919` |
//! | `isRaining` | `rain > 0.2` | `Level.java:947` |
//! | `isThundering` | `thunder > 0.9` | `Level.java:943` |
//! | sky rain darken | `×(1 - r·0.5, 1 - r·0.5, 1 - r·0.4)` | `AtmosphericFogEnvironment.java:51-54` |
//! | sky thunder darken | `×(1 - t·0.5)` all three | `:57-59` |
//! | sky-light floor | `0.24`, alpha `0.3125` rain / `0.52734375` thunder | `WeatherAttributes.java:19,30` |
//! | lightning flash colour | `(204, 204, 255)` at `0.22` | `ClientLevel.java:264-266` |
//! | lightning flash light | `SKY_LIGHT_FACTOR` forced to `1.0` | `ClientLevel.java:268` |
//! | rain sound cadence | `rand(3) < rainSoundTime++` | `ClientLevel.java:384` |
//! | rain sound volumes | `0.2 @ 1.0` normal, `0.1 @ 0.5` from above | `ClientLevel.java:388-390` |
//!
//! # How to change it, and the gotchas
//!
//! * **`START_RAINING` sets the rain level to `0.0` and `STOP_RAINING` sets it to
//!   `1.0`.** That is not a transcription slip — it is vanilla's own inversion at
//!   `ClientPacketListener.java:1542-1545`, and [`WeatherState::apply_raining`]
//!   reproduces it deliberately. It is invisible in practice because
//!   `ServerLevel.java:783-791` sends a `RAIN_LEVEL_CHANGE` with the true level
//!   immediately after every start/stop, and another every tick while the level
//!   ramps by ±0.01 (`ServerLevel.java:762-768`). Do **not** "fix" it to the
//!   intuitive polarity without also deciding what happens on a server with
//!   `doWeatherCycle false`, where vanilla itself shows no rain after a bare
//!   `/weather rain`.
//! * **Thunder is multiplied by rain, always.** [`WeatherState::thunder_level`]
//!   is the *composed* value; the raw wire field is
//!   [`WeatherState::raw_thunder_level`]. Reading the raw one into a darkening
//!   term produces a black sky in clear weather the moment a server sends a
//!   stale non-zero thunder level, which it does on join
//!   (`PlayerList.java:654-656` sends all three unconditionally).
//! * **Rain versus snow is decided per column, and today every column answers
//!   `Rain`.** The predicate ([`precipitation_for_temperature`]) is vanilla's and
//!   is exercised by unit tests, but its *input* — the biome's `temperature` and
//!   `has_precipitation` — is not decoded by this client. See
//!   [`WeatherProbe::precipitation`] for exactly which two NBT fields to add to
//!   `crates/protocol/v770/src/packets/registry.rs` to close it.

use bytemuck::{Pod, Zeroable};

/// What falls in a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precipitation {
    /// Nothing: no precipitation in this biome, or no sky above the column.
    None,
    /// Rain — sampled from `textures/environment/rain.png`.
    Rain,
    /// Snow — sampled from `textures/environment/snow.png`, drifting rather than
    /// falling straight, and 20% more transparent.
    Snow,
}

/// One edge of the perpendicular-offset lookup table (`WeatherEffectRenderer.java:43`).
pub const RAIN_TABLE_SIZE: i32 = 32;

/// The table's centre, i.e. the camera's own column (`:44`).
pub const HALF_RAIN_TABLE_SIZE: i32 = 16;

/// Vanilla's `Biome.warmEnoughToRain` threshold (`Biome.java:175-176`).
///
/// A **height-adjusted** temperature at or above this is rain; below it is snow.
/// `Biome.getHeightAdjustedTemperature` subtracts from the base temperature above
/// sea level, which is why a mountain peak in a plains biome snows.
pub const WARM_ENOUGH_TO_RAIN: f32 = 0.15;

/// Vanilla's `getPrecipitationAt` decision, given a biome's climate at a
/// position (`Biome.java:104-108`).
///
/// `temperature` must already be height-adjusted; see [`height_adjusted_temperature`].
#[must_use]
pub fn precipitation_for_temperature(has_precipitation: bool, temperature: f32) -> Precipitation {
    if !has_precipitation {
        Precipitation::None
    } else if temperature >= WARM_ENOUGH_TO_RAIN {
        Precipitation::Rain
    } else {
        Precipitation::Snow
    }
}

/// Vanilla's `Biome.getHeightAdjustedTemperature` (`Biome.java:110-121`): above
/// sea level the temperature falls off with height, so a peak snows while the
/// valley below it rains.
///
/// Vanilla's expression is
/// `temperature - (y - seaLevel) * 0.05 / 40` seeded through a fixed noise
/// offset; the noise term is a per-position `BiomeManager` sample this client has
/// no generator for, so only the deterministic height falloff is reproduced. The
/// omission moves the rain/snow line by at most the noise amplitude (±0.05 °,
/// i.e. one block of altitude), never the branch itself.
#[must_use]
pub fn height_adjusted_temperature(base_temperature: f32, y: i32, sea_level: i32) -> f32 {
    let above = (y - sea_level) as f32;
    if above > 0.0 {
        base_temperature - above * 0.05 / 40.0
    } else {
        base_temperature
    }
}

/// The world's rain and thunder levels, and the lightning-flash countdown.
///
/// Cheap and `Copy`: one of these is read per frame by the render side and
/// written by the net thread's router arm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeatherState {
    rain: f32,
    thunder: f32,
    /// Ticks of lightning flash left, vanilla's `ClientLevel.skyFlashTime`.
    flash_ticks: u32,
}

impl Default for WeatherState {
    /// Clear weather, no flash — which is also the pre-first-packet answer and
    /// what every headless/offline path gets.
    fn default() -> Self {
        Self {
            rain: 0.0,
            thunder: 0.0,
            flash_ticks: 0,
        }
    }
}

/// Ticks a single lightning bolt holds the sky flash for.
///
/// Vanilla's client sets `skyFlashTime = 2` on **every** tick a `LightningBolt`
/// has `life >= 0` (`LightningBolt.java:139-141`), and `life` starts at `2`
/// (`:45`), so one flash covers ticks 0, 1, 2 plus the 2-tick decay — five ticks
/// from the spawn. A bolt also re-flashes `flashes = rand(3) + 1` times by
/// resetting `life = 1` (`:47`, `:131-134`), which needs the bolt's own per-tick
/// state; this client only sees the spawn, so it flashes **once**. Documented as
/// a known shortfall rather than padded to hide it.
pub const LIGHTNING_FLASH_TICKS: u32 = 5;

/// Vanilla's lightning-flash sky tint, `ARGB.color(204, 204, 255)`
/// (`ClientLevel.java:264`).
pub const LIGHTNING_FLASH_COLOR: [f32; 3] = [204.0 / 255.0, 204.0 / 255.0, 255.0 / 255.0];

/// How far toward [`LIGHTNING_FLASH_COLOR`] the sky lerps during a flash
/// (`ClientLevel.java:266`).
pub const LIGHTNING_FLASH_MIX: f32 = 0.22;

/// The floor `SKY_LIGHT_FACTOR` is blended toward under rain and thunder
/// (`WeatherAttributes.java:19` and `:30` — the same `0.24` in both).
pub const WEATHER_SKY_LIGHT_FLOOR: f32 = 0.24;

/// Rain's blend weight toward [`WEATHER_SKY_LIGHT_FLOOR`]
/// (`WeatherAttributes.java:19`, `FloatWithAlpha(0.24, 0.3125)`).
pub const RAIN_SKY_LIGHT_ALPHA: f32 = 0.3125;

/// Thunder's blend weight toward [`WEATHER_SKY_LIGHT_FLOOR`]
/// (`WeatherAttributes.java:30`, `FloatWithAlpha(0.24, 0.52734375)`).
pub const THUNDER_SKY_LIGHT_ALPHA: f32 = 0.527_343_75;

impl WeatherState {
    /// Clear weather.
    #[must_use]
    pub fn clear() -> Self {
        Self::default()
    }

    /// Apply a `GAME_EVENT` `START_RAINING` (`true`) / `STOP_RAINING` (`false`).
    ///
    /// **This looks backwards and is not.** `ClientPacketListener.java:1542-1545`
    /// sets the level to `0.0` on start and `1.0` on stop; see the module doc for
    /// why reproducing it is correct rather than a bug being copied.
    pub fn apply_raining(&mut self, raining: bool) {
        self.rain = if raining { 0.0 } else { 1.0 };
    }

    /// Apply a `RAIN_LEVEL_CHANGE`. Clamped as `Level.setRainLevel` clamps
    /// (`Level.java:932-936`).
    pub fn apply_rain_level(&mut self, level: f32) {
        self.rain = clamp01(level);
    }

    /// Apply a `THUNDER_LEVEL_CHANGE`. Clamped as `Level.setThunderLevel` clamps
    /// (`Level.java:921-925`).
    pub fn apply_thunder_level(&mut self, level: f32) {
        self.thunder = clamp01(level);
    }

    /// Start a lightning flash — call once per `lightning_bolt` spawn.
    pub fn flash(&mut self) {
        self.flash_ticks = LIGHTNING_FLASH_TICKS;
    }

    /// Count one tick of the flash down, as `ClientLevel.java:303-305` does.
    pub fn tick_flash(&mut self) {
        self.flash_ticks = self.flash_ticks.saturating_sub(1);
    }

    /// Whether a lightning flash is currently lighting the sky.
    #[must_use]
    pub const fn flashing(&self) -> bool {
        self.flash_ticks > 0
    }

    /// `Level.getRainLevel` (`Level.java:928-930`), already clamped.
    ///
    /// No `oRainLevel`/`rainLevel` interpolation pair: vanilla's `setRainLevel`
    /// assigns *both*, so its own `Mth.lerp(a, o, n)` is a no-op on any
    /// server-driven change — the smoothness comes from the server's ±0.01/tick
    /// ramp (`ServerLevel.java:762-768`), not from client interpolation.
    #[must_use]
    pub const fn rain_level(&self) -> f32 {
        self.rain
    }

    /// `Level.getThunderLevel` (`Level.java:918-920`) — the raw level **times**
    /// the rain level, which is the value every consumer wants.
    #[must_use]
    pub fn thunder_level(&self) -> f32 {
        self.thunder * self.rain
    }

    /// The un-composed wire field, for tests and diagnostics only. Prefer
    /// [`thunder_level`](Self::thunder_level) everywhere else.
    #[must_use]
    pub const fn raw_thunder_level(&self) -> f32 {
        self.thunder
    }

    /// `Level.isRaining` (`Level.java:946-948`): the gate vanilla uses for
    /// "should anything actually be falling", `rain > 0.2`.
    ///
    /// Note this is **not** the gate the renderer uses. `WeatherEffectRenderer`
    /// extracts on `intensity > 0.0` (`:64`), so the first 20 ticks of a ramp draw
    /// faint rain before `isRaining` flips; the two thresholds are genuinely
    /// different and conflating them makes rain pop in.
    #[must_use]
    pub fn is_raining(&self) -> bool {
        self.rain > 0.2
    }

    /// Whether the weather pass has anything to draw at all — vanilla's own
    /// extraction gate (`WeatherEffectRenderer.java:64`).
    #[must_use]
    pub fn any_precipitation(&self) -> bool {
        self.rain > 0.0
    }
}

fn clamp01(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

/// Vanilla's `applyWeatherDarken` (`AtmosphericFogEnvironment.java:50-62`),
/// applied to a **gamma-space** sRGB triple in `0.0..=1.0`.
///
/// Gamma space is not an approximation here: vanilla's `ARGB.scaleRGB` multiplies
/// the 0..255 byte channels directly (`ARGB.java:108-115`), and its framebuffer
/// is not colour-managed. Callers holding linear colour must round-trip
/// (`srgb_to_linear(darken(linear_to_srgb(c), …))`) — the same discipline
/// `model.wgsl` documents for tint and shade.
///
/// Blue is scaled *less* than red and green by rain (0.4 vs 0.5), which is what
/// makes a rainy sky read as cold rather than merely dim. Thunder scales all three
/// equally.
#[must_use]
pub fn weather_darken_srgb(color: [f32; 3], rain_level: f32, thunder_level: f32) -> [f32; 3] {
    let mut out = color;
    if rain_level > 0.0 {
        let rgb_scale = 1.0 - rain_level * 0.5;
        let blue_scale = 1.0 - rain_level * 0.4;
        out = [
            out[0] * rgb_scale,
            out[1] * rgb_scale,
            out[2] * blue_scale,
        ];
    }
    if thunder_level > 0.0 {
        let scale = 1.0 - thunder_level * 0.5;
        out = [out[0] * scale, out[1] * scale, out[2] * scale];
    }
    out
}

/// The lightning-flash sky lerp (`ClientLevel.java:264-266`), in gamma space for
/// the same reason as [`weather_darken_srgb`] — vanilla's `ARGB.srgbLerp`
/// interpolates the byte channels (`ARGB.java:155-161`).
///
/// Applied **before** the weather darkening in vanilla's layer order: the flash
/// layer is added by `ClientLevel.addEnvironmentAttributeLayers` and the weather
/// layers by `WeatherAttributes.addBuiltinLayers`, both onto `SKY_COLOR`, with the
/// weather layers registered later (`ClientLevel.java:258` builds on
/// `addDefaultLayers`, which is where the weather layers live).
#[must_use]
pub fn lightning_flash_srgb(color: [f32; 3], flashing: bool) -> [f32; 3] {
    if !flashing {
        return color;
    }
    let a = LIGHTNING_FLASH_MIX;
    [
        color[0] + (LIGHTNING_FLASH_COLOR[0] - color[0]) * a,
        color[1] + (LIGHTNING_FLASH_COLOR[1] - color[1]) * a,
        color[2] + (LIGHTNING_FLASH_COLOR[2] - color[2]) * a,
    ]
}

/// [`weather_darken_srgb`] for a caller holding **linear** RGB — which is every
/// caller in this tree, because [`crate::fog::FogSettings`]'s colours are linear.
///
/// This exists so no caller has to remember the round-trip. The bug it forecloses
/// is a measured one: a fog mix sitting *one line* outside `model.wgsl`'s gamma
/// round-trip came out 49% too strong. Doing this darkening in linear light would
/// pull both scale factors toward 1.0 and produce a rainy sky that is barely
/// darker than a clear one — a "it got darker" gate would still pass.
///
/// The two vanilla `scaleRGB` calls are folded into one multiply, which is exact
/// up to vanilla's own byte rounding between them.
#[must_use]
pub fn weather_darken_linear(linear: [f32; 3], rain_level: f32, thunder_level: f32) -> [f32; 3] {
    if rain_level <= 0.0 && thunder_level <= 0.0 {
        return linear;
    }
    let rain = rain_level.max(0.0);
    let thunder = thunder_level.max(0.0);
    let rgb = (1.0 - rain * 0.5) * (1.0 - thunder * 0.5);
    let blue = (1.0 - rain * 0.4) * (1.0 - thunder * 0.5);
    crate::fog::multiply_gamma(linear, [rgb, rgb, blue])
}

/// [`lightning_flash_srgb`] for a caller holding **linear** RGB. Same reason as
/// [`weather_darken_linear`]: vanilla's `ARGB.srgbLerp` interpolates gamma bytes,
/// so lerping in linear light lands somewhere else.
#[must_use]
pub fn lightning_flash_linear(linear: [f32; 3], flashing: bool) -> [f32; 3] {
    if !flashing {
        return linear;
    }
    let gamma = [
        crate::fog::linear_to_srgb_f32(linear[0]),
        crate::fog::linear_to_srgb_f32(linear[1]),
        crate::fog::linear_to_srgb_f32(linear[2]),
    ];
    let lit = lightning_flash_srgb(gamma, true);
    [
        crate::fog::srgb_to_linear_f32(lit[0]),
        crate::fog::srgb_to_linear_f32(lit[1]),
        crate::fog::srgb_to_linear_f32(lit[2]),
    ]
}

/// The `SKY_LIGHT_FACTOR` a `sky_darken` becomes under this weather —
/// `WeatherAttributes`' two `FloatModifier.ALPHA_BLEND` layers, applied in
/// vanilla's own order (`WeatherAttributes.java:43-63`).
///
/// The layering is subtler than "blend twice", and the subtlety is load-bearing:
/// vanilla splits the two weights so they do **not** double-count, taking
/// `thunder = thunderLevel` and `rain = rainLevel - thunderLevel`
/// (`WeatherAttributes.java:49-50`). At full thunder the rain weight is therefore
/// `0`, and only the thunder layer applies — otherwise a full storm would be
/// darkened twice and undershoot the floor.
///
/// A flash overrides everything to `1.0` (`ClientLevel.java:268`), which is what
/// makes lightning read as a *brightening* of the world and not just of the sky.
///
/// Returns a factor in `[WEATHER_SKY_LIGHT_FLOOR, 1.0]` for any `sky_darken` in
/// that range, so the result is safe to hand straight to
/// [`crate::light::light_term`].
#[must_use]
pub fn weather_sky_light_factor(sky_darken: f32, weather: &WeatherState) -> f32 {
    if weather.flashing() {
        return 1.0;
    }
    let thunder = weather.thunder_level();
    let rain = (weather.rain_level() - thunder).max(0.0);
    let mut factor = sky_darken;
    if rain > 0.0 {
        // `FloatModifier.ALPHA_BLEND` toward the modifier's own value at the
        // modifier's alpha, then the *state* lerp by how much rain there is.
        let modified = blend(factor, WEATHER_SKY_LIGHT_FLOOR, RAIN_SKY_LIGHT_ALPHA);
        factor += (modified - factor) * rain;
    }
    if thunder > 0.0 {
        let modified = blend(factor, WEATHER_SKY_LIGHT_FLOOR, THUNDER_SKY_LIGHT_ALPHA);
        factor += (modified - factor) * thunder;
    }
    factor
}

/// `FloatModifier.ALPHA_BLEND`: `from` toward `to` by `alpha`.
fn blend(from: f32, to: f32, alpha: f32) -> f32 {
    from + (to - from) * alpha
}

/// The perpendicular half-offsets every column's quad is built from
/// (`WeatherEffectRenderer.java:50-60`), indexed `z * 32 + x` over the camera's
/// own 32×32 neighbourhood.
///
/// Each entry is the unit vector **perpendicular** to the camera→column direction
/// in the XZ plane, so the quad faces the camera without any per-column
/// normalisation at draw time. The camera's own cell (`x == z == 16`) divides by a
/// zero distance and is `NaN` in vanilla too; it is replaced with a fixed
/// `(-1, 0)` here so a camera standing exactly on a column centre cannot produce
/// a `NaN` vertex. That divergence is deliberate and it is invisible: the column
/// containing the camera is a 1×1 sliver directly under the eye.
#[must_use]
pub fn column_offset_table() -> Vec<[f32; 2]> {
    let n = RAIN_TABLE_SIZE;
    let mut table = vec![[0.0f32; 2]; (n * n) as usize];
    for z in 0..n {
        for x in 0..n {
            let dx = (x - HALF_RAIN_TABLE_SIZE) as f32;
            let dz = (z - HALF_RAIN_TABLE_SIZE) as f32;
            let distance = (dx * dx + dz * dz).sqrt();
            table[(z * n + x) as usize] = if distance > 0.0 {
                [-dz / distance, dx / distance]
            } else {
                [-1.0, 0.0]
            };
        }
    }
    table
}

/// One column of falling precipitation, before it is turned into a quad.
///
/// This is vanilla's `WeatherEffectRenderer.ColumnInstance` (`:232`) with the
/// packed light byte resolved to a scalar term — see [`WeatherProbe::light`] for
/// why the resolve happens on the CPU rather than by binding a lightmap texture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeatherColumn {
    /// Block X of the column.
    pub x: i32,
    /// Block Z of the column.
    pub z: i32,
    /// Bottom of the drawn span, in blocks.
    pub bottom_y: i32,
    /// Top of the drawn span, in blocks.
    pub top_y: i32,
    /// Horizontal texture offset. `0.0` for rain (which falls straight down);
    /// a per-column random walk for snow, which is what makes snow drift.
    pub u_offset: f32,
    /// Vertical texture offset — the scroll that animates the fall.
    pub v_offset: f32,
    /// The already-resolved lightmap term in `0.0..=1.0`.
    pub light: f32,
    /// Rain or snow. [`Precipitation::None`] columns are never built.
    pub kind: Precipitation,
}

/// What the extraction needs to know about the world, supplied by the shell.
///
/// Same seam idiom as `lodestone_shell::gpu::sources`: the renderer cannot reach
/// the world, and threading it through every call would touch every caller.
pub trait WeatherProbe {
    /// The `MOTION_BLOCKING` height of column `(x, z)` — the y of the first
    /// non-passable block from the top, i.e. where rain lands.
    ///
    /// `None` means "unknown" (an unloaded chunk, or a client with no heightmap
    /// plumbing). Vanilla clamps the drawn span to
    /// `max(camera_y ± radius, terrain_height)` (`WeatherEffectRenderer.java:74-76`);
    /// an unknown height falls back to the unclamped `camera_y ± radius` span,
    /// which draws rain below ground too. That is not visible — the pass is
    /// depth-tested against terrain, so sub-surface fragments are occluded — but
    /// it does cost vertices, and it is the reason [`precipitation`] gets a
    /// sky-visibility say as well.
    ///
    /// [`precipitation`]: WeatherProbe::precipitation
    fn column_top(&self, x: i32, z: i32) -> Option<i32>;

    /// What falls at `(x, y, z)`: vanilla's `ClientLevel.getPrecipitationAt`
    /// (`ClientLevel.java:396-…`), which is the conjunction of three things —
    /// the chunk being loaded, the position seeing sky, and the biome's climate.
    ///
    /// # The biome half is not reachable in this client, and that is the honest gap
    ///
    /// Biome **holder ids** do reach the client — `lodestone_world::ChunkSection`
    /// stores a 4×4×4 biome palette and `Section::biome_at_block` reads it. What
    /// does not reach it is the biome's *climate*: `ClientRegistries` keeps only
    /// `minecraft:visual/sky_color` per biome
    /// (`crates/protocol/v770/src/packets/registry.rs:285`), so there is no
    /// `temperature` and no `has_precipitation` to feed
    /// [`precipitation_for_temperature`] with.
    ///
    /// The two fields are already on the wire and already in the hermetic
    /// fixture (`crates/protocol/v770/tests/registry_data.rs:227-228` asserts
    /// `has_precipitation: Byte(1)` and `temperature: Float(0.8)` in a real
    /// entry), so closing this is a `biome_climates()` table alongside
    /// `biome_sky_colors()` and one new `ClientEvent` field — **not** a new
    /// packet and not a proxy. Until then every implementation in the tree
    /// answers [`Precipitation::Rain`] wherever it answers at all, and snow is
    /// reachable only from a test probe. Said plainly rather than guessed at
    /// from block ids or Y level.
    fn precipitation(&self, x: i32, y: i32, z: i32) -> Precipitation;

    /// The lightmap term at `(x, y, z)` in `0.0..=1.0`.
    ///
    /// Resolved here, on the CPU, rather than by binding a lightmap texture to the
    /// weather pass: it is one sample per *column* (a few thousand at most, once
    /// per frame) against a shell-side sampler that already exists for particles
    /// and entities (`EntityLightSource`), and it keeps the pass at two bind
    /// groups. See [`crate::light::light_term`] for the term itself.
    fn light(&self, x: i32, y: i32, z: i32) -> f32;
}

/// A probe that knows nothing: no heights, rain everywhere, full bright.
///
/// This is what the offline demo and every headless test gets, and it draws rain
/// — deliberately. A probe that answered [`Precipitation::None`] would make
/// "nothing on screen" the default and hide a wiring break behind an
/// indistinguishable empty frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct FullBrightRainProbe;

impl WeatherProbe for FullBrightRainProbe {
    fn column_top(&self, _x: i32, _z: i32) -> Option<i32> {
        None
    }
    fn precipitation(&self, _x: i32, _y: i32, _z: i32) -> Precipitation {
        Precipitation::Rain
    }
    fn light(&self, _x: i32, _y: i32, _z: i32) -> f32 {
        1.0
    }
}

/// Vanilla's default `weatherRadius` option (`Options.java`'s
/// `weatherRadius`, exposed in this shell's options menu as
/// "Weather Effect Radius" — `crates/lodestone-shell/src/menu/options.rs:496`).
///
/// 10 columns each way is 441 columns, which is what keeps this a cheap pass. It
/// must stay `<= HALF_RAIN_TABLE_SIZE` or a column indexes outside
/// [`column_offset_table`]; [`extract_columns`] clamps rather than panicking.
pub const DEFAULT_WEATHER_RADIUS: i32 = 10;

/// Vanilla's per-column random seed (`WeatherEffectRenderer.java:80`).
///
/// Reproduced with wrapping arithmetic because the Java expression overflows
/// `int` for most `x`/`z` and relies on it; `as i32` on a widened product would
/// give a different (and position-dependent) answer.
#[must_use]
pub fn column_seed(x: i32, z: i32) -> i32 {
    let a = x
        .wrapping_mul(x)
        .wrapping_mul(3121)
        .wrapping_add(x.wrapping_mul(45_238_971));
    let b = z
        .wrapping_mul(z)
        .wrapping_mul(418_711)
        .wrapping_add(z.wrapping_mul(13_761));
    a ^ b
}

/// A tiny deterministic PRNG standing in for vanilla's `RandomSource`.
///
/// Vanilla seeds a `LegacyRandomSource` per column and pulls one `nextFloat` for
/// rain, or two `nextDouble`s and two `nextGaussian`s for snow. Java's exact
/// `Random` stream is not reproduced — nothing observable depends on matching it,
/// because these values only decide *which* offset a given column gets, not the
/// distribution — but it must be **deterministic per column** or every column
/// re-rolls its phase each frame and the rain visibly seethes. That is the whole
/// contract, and it is what the unit tests pin.
#[derive(Clone, Copy, Debug)]
pub struct ColumnRandom(u64);

impl ColumnRandom {
    /// Seed from a [`column_seed`].
    #[must_use]
    pub fn new(seed: i32) -> Self {
        // SplitMix64's seeding constant; any odd-multiplier mixer would do.
        Self((seed as i64 as u64) ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0.0..1.0`.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u32 << 24) as f32
    }

    /// Roughly normal, mean 0, unit variance — the sum-of-uniforms
    /// approximation, which is enough for a drift offset.
    pub fn next_gaussian(&mut self) -> f32 {
        (self.next_f32() + self.next_f32() + self.next_f32() - 1.5) * 2.0
    }
}

/// Build a rain column (`WeatherEffectRenderer.createRainColumnInstance`, `:163-172`).
#[must_use]
pub fn rain_column(
    random: &mut ColumnRandom,
    game_time: i64,
    x: i32,
    bottom_y: i32,
    top_y: i32,
    z: i32,
    light: f32,
    partial_ticks: f32,
) -> WeatherColumn {
    let wrapped_ticks = (game_time & 131_071) as i32;
    let tick_offset = column_seed(x, z) & 0xFF;
    let speed = 3.0 + random.next_f32();
    let offset = -((wrapped_ticks + tick_offset) as f32 + partial_ticks) / 32.0 * speed;
    WeatherColumn {
        x,
        z,
        bottom_y,
        top_y,
        u_offset: 0.0,
        v_offset: offset % 32.0,
        light,
        kind: Precipitation::Rain,
    }
}

/// Build a snow column (`WeatherEffectRenderer.createSnowColumnInstance`, `:174-184`).
///
/// The light boost is vanilla's: each half of the packed byte becomes
/// `(l * 3 + 15) / 4`, i.e. snow is lit three-quarters of the way to full bright
/// so it stays visible at night. Applied here to the already-resolved *term*
/// rather than to the nibbles, which is a divergence — the real transform is
/// non-linear in the term because [`crate::light::brightness`] is concave — so it
/// is applied as the same affine map on the **level** by inverting nothing and
/// instead lifting the term directly. Snow at night therefore reads slightly
/// darker here than in vanilla; noted rather than silently approximated, and
/// cheap to fix the day a probe hands back the raw packed byte.
#[must_use]
pub fn snow_column(
    random: &mut ColumnRandom,
    game_time: i64,
    x: i32,
    bottom_y: i32,
    top_y: i32,
    z: i32,
    light: f32,
    partial_ticks: f32,
) -> WeatherColumn {
    let wrapped_ticks = (game_time & 131_071) as i32;
    let time = wrapped_ticks as f32 + partial_ticks;
    let u = random.next_f32() + time * 0.01 * random.next_gaussian();
    let v = random.next_f32() + time * random.next_gaussian() * 0.001;
    let v_scroll = -(((game_time & 511) as f32) + partial_ticks) / 512.0;
    WeatherColumn {
        x,
        z,
        bottom_y,
        top_y,
        u_offset: u,
        v_offset: v_scroll + v,
        light: ((light * 3.0 + 1.0) / 4.0).min(1.0),
        kind: Precipitation::Snow,
    }
}

/// Walk the `radius`-square around the camera and build one column per cell that
/// has precipitation — vanilla's `extractRenderState` (`:62-94`).
///
/// Returns columns **sorted rain-first**, so the pass can issue two instanced
/// draws with two textures and no per-fragment branch, exactly as vanilla issues
/// two `drawIndexed` calls (`:157-158`).
///
/// `radius` is clamped to `HALF_RAIN_TABLE_SIZE` because a wider radius would
/// index outside [`column_offset_table`]; vanilla's option maxes out below the
/// table size so the clamp is unreachable in practice, and it is here so a caller
/// passing a bad radius gets less rain rather than a panic.
#[must_use]
pub fn extract_columns(
    weather: &WeatherState,
    radius: i32,
    game_time: i64,
    partial_ticks: f32,
    camera: [f64; 3],
    probe: &dyn WeatherProbe,
) -> Vec<WeatherColumn> {
    if !weather.any_precipitation() {
        return Vec::new();
    }
    let radius = radius.clamp(0, HALF_RAIN_TABLE_SIZE - 1);
    let cam_x = camera[0].floor() as i32;
    let cam_y = camera[1].floor() as i32;
    let cam_z = camera[2].floor() as i32;

    let mut columns = Vec::new();
    for z in (cam_z - radius)..=(cam_z + radius) {
        for x in (cam_x - radius)..=(cam_x + radius) {
            // Vanilla clamps both ends of the span up to the terrain height, so a
            // column over a mountain draws only above the peak. With no heightmap
            // the span is the plain camera-centred cube.
            // Resolved **once** per column and reused for the light sample below.
            // It used to be asked twice for the same `(x, z)`, which doubled the
            // probe traffic — and every shell-side probe query is a world lock
            // (`lodestone_shell::app::weather`), so the second call was 441 locks
            // a frame for a value already in hand.
            let terrain = probe.column_top(x, z);
            let (bottom_y, top_y) = match terrain {
                Some(terrain) => ((cam_y - radius).max(terrain), (cam_y + radius).max(terrain)),
                None => (cam_y - radius, cam_y + radius),
            };
            if top_y == bottom_y {
                continue;
            }
            let kind = probe.precipitation(x, cam_y, z);
            if kind == Precipitation::None {
                continue;
            }
            // Vanilla samples light at `max(camera_y, terrain_height)`, i.e. at the
            // top of the column, never at the camera's own y when standing in a
            // hole — otherwise a player in a one-block pit sees pitch-black rain.
            let light_y = terrain.map_or(cam_y, |t| cam_y.max(t));
            let light = probe.light(x, light_y, z);
            let mut random = ColumnRandom::new(column_seed(x, z));
            columns.push(match kind {
                Precipitation::Snow => snow_column(
                    &mut random,
                    game_time,
                    x,
                    bottom_y,
                    top_y,
                    z,
                    light,
                    partial_ticks,
                ),
                _ => rain_column(
                    &mut random,
                    game_time,
                    x,
                    bottom_y,
                    top_y,
                    z,
                    light,
                    partial_ticks,
                ),
            });
        }
    }
    columns.sort_by_key(|c| u8::from(c.kind == Precipitation::Snow));
    columns
}

/// Vanilla's rain max alpha (`WeatherEffectRenderer.java:133`).
pub const RAIN_MAX_ALPHA: f32 = 1.0;

/// Vanilla's snow max alpha (`:134`) — snow is drawn 20% more transparent.
pub const SNOW_MAX_ALPHA: f32 = 0.8;

/// One column's quad, ready for the vertex buffer.
///
/// 48 bytes, three `vec4`s, all `f32` — no padding needed and nothing for a
/// `bytemuck` derive to complain about.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct WeatherInstance {
    /// `rel_x, rel_z` (camera-relative column centre) and `y0, y1` (camera-relative
    /// bottom and top).
    pub base: [f32; 4],
    /// `half_x, half_z` (the perpendicular half-offset from
    /// [`column_offset_table`]), `u_offset`, and `v0`.
    pub axis: [f32; 4],
    /// `v1`, `alpha`, `light`, and a spare.
    pub shade: [f32; 4],
}

/// Turn one column into its quad (`WeatherEffectRenderer.renderInstances`, `:186-222`).
///
/// `offsets` must be [`column_offset_table`]. The distance fade is vanilla's:
/// alpha runs from the kind's max at the camera to `0.5` at the radius, then the
/// whole thing is scaled by the rain level — so a column both far away *and* in
/// light rain is doubly faint, which is the behaviour that makes a ramp read as a
/// ramp instead of as a wall of rain switching on.
#[must_use]
pub fn column_instance(
    column: &WeatherColumn,
    camera: [f64; 3],
    offsets: &[[f32; 2]],
    radius: i32,
    intensity: f32,
) -> WeatherInstance {
    let max_alpha = match column.kind {
        Precipitation::Snow => SNOW_MAX_ALPHA,
        _ => RAIN_MAX_ALPHA,
    };
    let rel_x = (f64::from(column.x) + 0.5 - camera[0]) as f32;
    let rel_z = (f64::from(column.z) + 0.5 - camera[2]) as f32;
    let radius_sq = (radius * radius).max(1) as f32;
    let distance_sq = rel_x * rel_x + rel_z * rel_z;
    let t = (distance_sq / radius_sq).min(1.0);
    let alpha = (max_alpha + (0.5 - max_alpha) * t) * intensity;

    // The table index is the column's offset from the camera's own cell, biased
    // into `0..32`. Derived from the *same* floor of the camera position the
    // extraction used, so a column can never index outside the table.
    let ix = column.x - (camera[0].floor() as i32) + HALF_RAIN_TABLE_SIZE;
    let iz = column.z - (camera[2].floor() as i32) + HALF_RAIN_TABLE_SIZE;
    let idx = (iz * RAIN_TABLE_SIZE + ix).clamp(0, (offsets.len() as i32) - 1) as usize;
    let half_x = offsets[idx][0] / 2.0;
    let half_z = offsets[idx][1] / 2.0;

    let y0 = (f64::from(column.bottom_y) - camera[1]) as f32;
    let y1 = (f64::from(column.top_y) - camera[1]) as f32;
    // V grows *downward* with world height in vanilla (`v0` is taken from the
    // bottom and applied to the top vertex, `:214-219`), which is what makes the
    // texture appear to fall rather than rise. Preserved exactly.
    let v0 = column.bottom_y as f32 * 0.25 + column.v_offset;
    let v1 = column.top_y as f32 * 0.25 + column.v_offset;

    WeatherInstance {
        base: [rel_x, rel_z, y0, y1],
        axis: [half_x, half_z, column.u_offset, v0],
        shade: [v1, alpha, column.light, 0.0],
    }
}

/// How many of a rain-first-sorted column list are rain.
#[must_use]
pub fn rain_count(columns: &[WeatherColumn]) -> usize {
    columns
        .iter()
        .take_while(|c| c.kind != Precipitation::Snow)
        .count()
}

/// The looping rain ambience, driven exactly as `ClientLevel.tickWeatherEffects`
/// drives it (`ClientLevel.java:383-392`).
///
/// Not a true looped voice: vanilla's rain is a **repeated one-shot** gated on a
/// counter, and `weather.rain` is an 8-sample set
/// (`ambient/weather/rain{1..8}.ogg`, confirmed present in the 26.2 asset index)
/// picked per play. Reproducing the cadence therefore needs no looping API at all
/// — `lodestone_shell::audio::ShellAudio::play_sound` is exactly the right shape
/// — and a real loop would be *less* faithful, because it would lose the sample
/// variation.
#[derive(Clone, Copy, Debug, Default)]
pub struct RainAmbience {
    /// Vanilla's `rainSoundTime`: ticks since the last play, compared against a
    /// `rand(3)` so the gap averages about two ticks once it is warm.
    counter: u32,
}

/// One rain-ambience play the caller should submit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RainSound {
    /// The `sounds.json` event path, namespace already stripped —
    /// `weather.rain` or `weather.rain.above`.
    pub name: &'static str,
    /// Volume (`0.2` normal, `0.1` from above).
    pub volume: f32,
    /// Pitch (`1.0` normal, `0.5` from above).
    pub pitch: f32,
}

/// `weather.rain` played at the listener's own level (`ClientLevel.java:390`).
pub const RAIN_SOUND_NEAR: RainSound = RainSound {
    name: "weather.rain",
    volume: 0.2,
    pitch: 1.0,
};

/// `weather.rain.above` — the muffled variant for rain landing on a roof above
/// the player (`ClientLevel.java:388`).
pub const RAIN_SOUND_ABOVE: RainSound = RainSound {
    name: "weather.rain.above",
    volume: 0.1,
    pitch: 0.5,
};

impl RainAmbience {
    /// Advance one tick and return a play, if this is a tick that plays.
    ///
    /// `landing_y` is the y rain is landing at (vanilla's `rainParticlePosition`,
    /// the block below the heightmap hit) and `roof_above` is whether the player
    /// themselves is under cover — vanilla's exact conjunction is
    /// `landing.y > camera.y + 1 && heightmap(camera) > floor(camera.y)`, i.e.
    /// rain is landing above the ear *and* there is something over the ear.
    ///
    /// `rain_roll` must be a fresh `0..3` value each tick; the caller owns the RNG
    /// so this stays deterministic under test. Returns `None` when there is no
    /// rain, when nothing is landing nearby, or on the ticks the counter swallows.
    pub fn tick(
        &mut self,
        weather: &WeatherState,
        landing: Option<[i32; 3]>,
        camera_y: f64,
        roof_above: bool,
        rain_roll: u32,
    ) -> Option<RainSound> {
        if !weather.any_precipitation() {
            // Vanilla never resets the counter on a dry tick because it never
            // reaches the counter at all; resetting here would make the first tick
            // of a shower always play, which is a pop.
            return None;
        }
        let landing = landing?;
        // Post-increment, as vanilla's `rainSoundTime++` is: the comparison sees
        // the *old* value, so the first eligible tick compares against 0 and can
        // only fire on a `rain_roll` of... nothing. It never fires immediately.
        let previous = self.counter;
        self.counter += 1;
        if rain_roll >= previous {
            return None;
        }
        self.counter = 0;
        let above = f64::from(landing[1]) > camera_y + 1.0 && roof_above;
        Some(if above { RAIN_SOUND_ABOVE } else { RAIN_SOUND_NEAR })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The inversion is deliberate. This test exists so that "fixing" it fails
    /// loudly and sends the reader to the module doc rather than to a bug report.
    #[test]
    fn start_raining_zeroes_the_level_and_stop_raining_fills_it() {
        let mut w = WeatherState::clear();
        w.apply_raining(true);
        assert_eq!(
            w.rain_level(),
            0.0,
            "vanilla's ClientPacketListener.java:1543 sets 0.0 on START_RAINING"
        );
        w.apply_raining(false);
        assert_eq!(
            w.rain_level(),
            1.0,
            "vanilla's ClientPacketListener.java:1545 sets 1.0 on STOP_RAINING"
        );
        // …and the control that the level is genuinely writable the other way,
        // which is what makes the inversion harmless in practice: the server
        // follows every start/stop with a RAIN_LEVEL_CHANGE.
        w.apply_rain_level(0.34);
        assert_eq!(w.rain_level(), 0.34);
    }

    #[test]
    fn thunder_is_composed_with_rain_and_levels_clamp() {
        let mut w = WeatherState::clear();
        w.apply_rain_level(0.5);
        w.apply_thunder_level(0.8);
        assert_eq!(w.raw_thunder_level(), 0.8);
        assert!(
            (w.thunder_level() - 0.4).abs() < 1e-6,
            "getThunderLevel multiplies by the rain level (Level.java:918)"
        );
        // A stale non-zero thunder level with no rain must be inert — this is the
        // join case (PlayerList.java:654-656 sends all three unconditionally).
        w.apply_rain_level(0.0);
        assert_eq!(w.thunder_level(), 0.0);
        w.apply_rain_level(5.0);
        assert_eq!(w.rain_level(), 1.0, "setRainLevel clamps");
        w.apply_thunder_level(f32::NAN);
        assert_eq!(w.raw_thunder_level(), 0.0, "NaN must not reach a uniform");
    }

    #[test]
    fn the_render_gate_and_the_is_raining_gate_are_different_thresholds() {
        let mut w = WeatherState::clear();
        w.apply_rain_level(0.1);
        assert!(
            w.any_precipitation(),
            "the renderer extracts on > 0.0 (WeatherEffectRenderer.java:64)"
        );
        assert!(
            !w.is_raining(),
            "but Level.isRaining is > 0.2 (Level.java:947) — conflating them pops"
        );
    }

    /// Magnitude, not sign. Both hypotheses are computed from vanilla's own
    /// constants and the measurement must land on one of them.
    #[test]
    fn rain_darkening_scales_blue_less_than_red_by_the_predicted_amount() {
        let white = [1.0, 1.0, 1.0];
        let out = weather_darken_srgb(white, 1.0, 0.0);
        // Right: red/green ×(1 - 1.0*0.5) = 0.5, blue ×(1 - 1.0*0.4) = 0.6.
        assert!((out[0] - 0.5).abs() < 1e-6, "red: {out:?}");
        assert!((out[1] - 0.5).abs() < 1e-6, "green: {out:?}");
        assert!((out[2] - 0.6).abs() < 1e-6, "blue: {out:?}");
        // The ratio is the part a caller can check without knowing the subject's
        // own colour: 0.6 / 0.5 = 1.2 if the two scales are the right way round,
        // 0.5 / 0.6 = 0.8333 if they are swapped. The two are 1.2 vs 0.833, which
        // no single-sided "it got darker" assertion can separate.
        let ratio = out[2] / out[0];
        assert!(
            (ratio - 1.2).abs() < 1e-5,
            "blue/red must be 1.2 (correct) and not 0.8333 (scales swapped): {ratio}"
        );
    }

    #[test]
    fn thunder_darkening_is_uniform_and_stacks_on_top_of_rain() {
        let out = weather_darken_srgb([1.0, 1.0, 1.0], 1.0, 1.0);
        // Rain first (0.5, 0.5, 0.6) then thunder ×0.5 → (0.25, 0.25, 0.3).
        assert!((out[0] - 0.25).abs() < 1e-6, "{out:?}");
        assert!((out[2] - 0.3).abs() < 1e-6, "{out:?}");
        // Thunder alone leaves the hue untouched.
        let t = weather_darken_srgb([1.0, 1.0, 1.0], 0.0, 1.0);
        assert!((t[0] - t[2]).abs() < 1e-6, "thunder must not tint: {t:?}");
    }

    /// The sky-light layering is the one place a plausible-looking wrong
    /// implementation ("blend for rain, then blend for thunder") differs
    /// measurably from vanilla's, so both hypotheses are computed here.
    #[test]
    fn sky_light_factor_does_not_double_count_a_full_storm() {
        let mut w = WeatherState::clear();
        w.apply_rain_level(1.0);
        w.apply_thunder_level(1.0);
        let got = weather_sky_light_factor(1.0, &w);
        // Correct: thunder = 1.0, so rain = 1.0 - 1.0 = 0.0 and only the thunder
        // layer runs. 1.0 + (0.24 - 1.0) * 0.52734375 = 0.599219.
        let correct = 1.0 + (WEATHER_SKY_LIGHT_FLOOR - 1.0) * THUNDER_SKY_LIGHT_ALPHA;
        // The naive double-blend: rain layer first (→ 0.7625), then thunder on
        // that (→ 0.487...). Materially darker, and "it got darker" passes both.
        let after_rain = 1.0 + (WEATHER_SKY_LIGHT_FLOOR - 1.0) * RAIN_SKY_LIGHT_ALPHA;
        let double_counted =
            after_rain + (WEATHER_SKY_LIGHT_FLOOR - after_rain) * THUNDER_SKY_LIGHT_ALPHA;
        assert!(
            (got - correct).abs() < 1e-5,
            "expected {correct} (WeatherAttributes.java:49-50 subtracts thunder from rain), \
             got {got}; the double-counted hypothesis is {double_counted}"
        );
        assert!(
            (correct - double_counted).abs() > 0.1,
            "the two hypotheses must be far enough apart for this gate to mean \
             anything: {correct} vs {double_counted}"
        );
    }

    #[test]
    fn clear_weather_leaves_the_sky_light_factor_exactly_alone() {
        let w = WeatherState::clear();
        for darken in [0.24_f32, 0.5, 1.0] {
            assert_eq!(
                weather_sky_light_factor(darken, &w),
                darken,
                "clear weather must be byte-identical to no weather at all"
            );
        }
        // Control: the detector works — a rainy state at the same input moves it.
        let mut rainy = WeatherState::clear();
        rainy.apply_rain_level(1.0);
        assert!(
            weather_sky_light_factor(1.0, &rainy) < 0.99,
            "the control must fail: rain has to move the factor, or the assertion \
             above is measuring nothing"
        );
    }

    #[test]
    fn a_lightning_flash_overrides_the_factor_to_full_bright_then_expires() {
        let mut w = WeatherState::clear();
        w.apply_rain_level(1.0);
        w.apply_thunder_level(1.0);
        let stormy = weather_sky_light_factor(0.24, &w);
        w.flash();
        assert_eq!(
            weather_sky_light_factor(0.24, &w),
            1.0,
            "ClientLevel.java:268 forces SKY_LIGHT_FACTOR to 1.0 during a flash"
        );
        for _ in 0..LIGHTNING_FLASH_TICKS {
            assert!(w.flashing());
            w.tick_flash();
        }
        assert!(!w.flashing(), "the flash must expire, not latch");
        assert_eq!(
            weather_sky_light_factor(0.24, &w),
            stormy,
            "and the storm's own factor must come back unchanged"
        );
    }

    #[test]
    fn the_flash_tint_moves_the_sky_toward_the_vanilla_flash_colour() {
        let black = [0.0, 0.0, 0.0];
        let lit = lightning_flash_srgb(black, true);
        // Predicted: 0.22 of the way from 0 to (0.8, 0.8, 1.0).
        assert!((lit[0] - 0.22 * 204.0 / 255.0).abs() < 1e-6, "{lit:?}");
        assert!((lit[2] - 0.22 * 1.0).abs() < 1e-6, "{lit:?}");
        assert!(lit[2] > lit[0], "the flash is blue-white, not white");
        assert_eq!(
            lightning_flash_srgb(black, false),
            black,
            "no flash must be exactly inert"
        );
    }

    /// The gamma round-trip is the whole reason [`weather_darken_linear`] exists,
    /// so it is measured against both hypotheses rather than asserted to be
    /// "darker". Mid-grey is the worst case for confusing the two.
    #[test]
    fn darkening_in_linear_light_would_be_measurably_too_weak() {
        // Linear 0.2 is sRGB ~0.4845. Full rain scales the gamma value by 0.5 to
        // ~0.2423, which is linear ~0.0466. Doing the same multiply in linear
        // light would give 0.1 — more than twice as bright.
        let linear = [0.2_f32, 0.2, 0.2];
        let out = weather_darken_linear(linear, 1.0, 0.0);
        let wrong = 0.2 * 0.5;
        let right = crate::fog::srgb_to_linear_f32(crate::fog::linear_to_srgb_f32(0.2) * 0.5);
        assert!(
            (out[0] - right).abs() < 1e-5,
            "expected the gamma-space answer {right}, got {}; the linear-space \
             hypothesis is {wrong}",
            out[0]
        );
        assert!(
            (right - wrong).abs() > 0.04,
            "the two hypotheses must be far apart for this to mean anything: \
             {right} vs {wrong}"
        );
        // Blue is still scaled less than red, in linear space too.
        assert!(out[2] > out[0], "{out:?}");
        // And clear weather is exactly inert — no round-trip error creeping in.
        assert_eq!(weather_darken_linear(linear, 0.0, 0.0), linear);
    }

    #[test]
    fn the_linear_flash_is_a_gamma_lerp_and_is_inert_when_not_flashing() {
        let linear = [0.05_f32, 0.05, 0.08];
        assert_eq!(lightning_flash_linear(linear, false), linear);
        let lit = lightning_flash_linear(linear, true);
        assert!(lit[0] > linear[0] && lit[2] > linear[2], "{lit:?}");
        let expected =
            crate::fog::srgb_to_linear_f32(lightning_flash_srgb(
                [crate::fog::linear_to_srgb_f32(0.05); 3],
                true,
            )[0]);
        assert!((lit[0] - expected).abs() < 1e-5, "{lit:?} vs {expected}");
    }

    #[test]
    fn precipitation_follows_vanillas_temperature_threshold() {
        assert_eq!(
            precipitation_for_temperature(false, 0.8),
            Precipitation::None,
            "has_precipitation false wins over any temperature"
        );
        assert_eq!(
            precipitation_for_temperature(true, 0.15),
            Precipitation::Rain,
            "the threshold is inclusive (Biome.java:176 is `>=`)"
        );
        assert_eq!(
            precipitation_for_temperature(true, 0.149),
            Precipitation::Snow
        );
        // A plains biome (temperature 0.8, the value in the real registry entry
        // at protocol/v770/tests/registry_data.rs:228) rains at sea level and
        // snows high enough up. 0.8 - (y - 63) * 0.00125 < 0.15 at y > 583.
        assert_eq!(
            precipitation_for_temperature(true, height_adjusted_temperature(0.8, 63, 63)),
            Precipitation::Rain
        );
        assert_eq!(
            precipitation_for_temperature(true, height_adjusted_temperature(0.8, 600, 63)),
            Precipitation::Snow,
            "the height falloff must be able to cross the threshold, or it is inert"
        );
    }

    #[test]
    fn the_offset_table_is_unit_length_perpendicular_and_never_nan() {
        let table = column_offset_table();
        assert_eq!(table.len(), 32 * 32);
        for (i, o) in table.iter().enumerate() {
            let len = (o[0] * o[0] + o[1] * o[1]).sqrt();
            assert!(
                (len - 1.0).abs() < 1e-5,
                "entry {i} is not unit length: {o:?} (len {len})"
            );
            assert!(o[0].is_finite() && o[1].is_finite(), "entry {i} is NaN");
        }
        // Perpendicularity, at one entry where the answer is unambiguous: the
        // column two cells to +X of the camera is offset (dx, dz) = (2, 0), whose
        // perpendicular is (0, 1).
        let e = table[(16 * 32 + 18) as usize];
        assert!(e[0].abs() < 1e-5 && (e[1] - 1.0).abs() < 1e-5, "{e:?}");
    }

    /// A per-column phase that changed between frames would make the rain seethe.
    /// This is the whole contract [`ColumnRandom`] has to meet.
    #[test]
    fn a_columns_phase_is_stable_across_frames_and_differs_between_columns() {
        let a = {
            let mut r = ColumnRandom::new(column_seed(12, -34));
            rain_column(&mut r, 1000, 12, 60, 80, -34, 1.0, 0.0)
        };
        let b = {
            let mut r = ColumnRandom::new(column_seed(12, -34));
            rain_column(&mut r, 1000, 12, 60, 80, -34, 1.0, 0.0)
        };
        assert_eq!(a, b, "the same column at the same tick must be identical");
        let c = {
            let mut r = ColumnRandom::new(column_seed(13, -34));
            rain_column(&mut r, 1000, 13, 60, 80, -34, 1.0, 0.0)
        };
        assert!(
            (a.v_offset - c.v_offset).abs() > 1e-6,
            "neighbouring columns must not share a phase, or the rain draws as \
             one flat sheet: {} vs {}",
            a.v_offset,
            c.v_offset
        );
        // And the animation must actually advance with the clock.
        let later = {
            let mut r = ColumnRandom::new(column_seed(12, -34));
            rain_column(&mut r, 1020, 12, 60, 80, -34, 1.0, 0.0)
        };
        assert!(
            (a.v_offset - later.v_offset).abs() > 1e-6,
            "20 ticks later the scroll must have moved"
        );
    }

    #[test]
    fn clear_weather_extracts_no_columns_and_rain_extracts_the_full_square() {
        let clear = WeatherState::clear();
        assert!(
            extract_columns(&clear, 3, 0, 0.0, [0.0, 64.0, 0.0], &FullBrightRainProbe).is_empty(),
            "clear weather must extract nothing"
        );
        // The control this absence needs: the same call with rain must not be
        // empty, or the assertion above passes for the wrong reason.
        let mut rainy = WeatherState::clear();
        rainy.apply_rain_level(0.05);
        let columns = extract_columns(&rainy, 3, 0, 0.0, [0.0, 64.0, 0.0], &FullBrightRainProbe);
        assert_eq!(
            columns.len(),
            7 * 7,
            "a radius-3 square is 7x7 columns; got {}",
            columns.len()
        );
        assert!(columns.iter().all(|c| c.kind == Precipitation::Rain));
        assert_eq!(rain_count(&columns), 49);
    }

    /// A probe with a real heightmap must actually change the drawn span, or the
    /// `column_top` lane is dead plumbing.
    #[test]
    fn a_terrain_height_clamps_the_span_and_a_flat_terrain_does_not() {
        struct Peak(i32);
        impl WeatherProbe for Peak {
            fn column_top(&self, _x: i32, _z: i32) -> Option<i32> {
                Some(self.0)
            }
            fn precipitation(&self, _x: i32, _y: i32, _z: i32) -> Precipitation {
                Precipitation::Rain
            }
            fn light(&self, _x: i32, _y: i32, _z: i32) -> f32 {
                1.0
            }
        }
        let mut rainy = WeatherState::clear();
        rainy.apply_rain_level(1.0);
        // Terrain at 200, camera at 64, radius 5: both ends clamp up to 200, so
        // the span collapses and nothing is drawn (we are inside the mountain).
        let inside = extract_columns(&rainy, 5, 0, 0.0, [0.0, 64.0, 0.0], &Peak(200));
        assert!(
            inside.is_empty(),
            "a camera below the terrain height must draw no rain, got {} columns",
            inside.len()
        );
        // Control: terrain below the camera leaves the span intact.
        let above = extract_columns(&rainy, 5, 0, 0.0, [0.0, 64.0, 0.0], &Peak(50));
        assert_eq!(above.len(), 11 * 11);
        assert_eq!(
            (above[0].bottom_y, above[0].top_y),
            (59, 69),
            "camera_y ± radius, both already above the terrain"
        );
    }

    #[test]
    fn snow_columns_sort_last_so_the_pass_can_issue_two_draws() {
        struct Mixed;
        impl WeatherProbe for Mixed {
            fn column_top(&self, _x: i32, _z: i32) -> Option<i32> {
                None
            }
            fn precipitation(&self, x: i32, _y: i32, _z: i32) -> Precipitation {
                if x % 2 == 0 {
                    Precipitation::Snow
                } else {
                    Precipitation::Rain
                }
            }
            fn light(&self, _x: i32, _y: i32, _z: i32) -> f32 {
                1.0
            }
        }
        let mut rainy = WeatherState::clear();
        rainy.apply_rain_level(1.0);
        let columns = extract_columns(&rainy, 2, 0, 0.0, [0.0, 64.0, 0.0], &Mixed);
        let rains = rain_count(&columns);
        assert!(rains > 0 && rains < columns.len(), "need both kinds present");
        assert!(
            columns[..rains]
                .iter()
                .all(|c| c.kind == Precipitation::Rain)
        );
        assert!(
            columns[rains..]
                .iter()
                .all(|c| c.kind == Precipitation::Snow)
        );
    }

    #[test]
    fn the_distance_fade_reaches_half_alpha_at_the_radius_and_scales_by_intensity() {
        let offsets = column_offset_table();
        let near = WeatherColumn {
            x: 0,
            z: 0,
            bottom_y: 60,
            top_y: 70,
            u_offset: 0.0,
            v_offset: 0.0,
            light: 1.0,
            kind: Precipitation::Rain,
        };
        let far = WeatherColumn { x: 10, ..near };
        let n = column_instance(&near, [0.5, 64.0, 0.5], &offsets, 10, 1.0);
        let f = column_instance(&far, [0.5, 64.0, 0.5], &offsets, 10, 1.0);
        // Predicted: the column under the camera is at distance 0, so alpha is the
        // rain max of 1.0. The column at the radius is (approximately) at t = 1,
        // so alpha is 0.5. Not "less than" — the two values.
        assert!((n.shade[1] - 1.0).abs() < 1e-4, "{:?}", n.shade);
        assert!((f.shade[1] - 0.5).abs() < 1e-2, "{:?}", f.shade);
        // Intensity is a straight multiply on top.
        let half = column_instance(&near, [0.5, 64.0, 0.5], &offsets, 10, 0.5);
        assert!((half.shade[1] - 0.5).abs() < 1e-4, "{:?}", half.shade);
    }

    #[test]
    fn snow_is_drawn_more_transparent_than_rain_by_exactly_the_vanilla_ratio() {
        let offsets = column_offset_table();
        let rain = WeatherColumn {
            x: 0,
            z: 0,
            bottom_y: 60,
            top_y: 70,
            u_offset: 0.0,
            v_offset: 0.0,
            light: 1.0,
            kind: Precipitation::Rain,
        };
        let snow = WeatherColumn {
            kind: Precipitation::Snow,
            ..rain
        };
        let r = column_instance(&rain, [0.5, 64.0, 0.5], &offsets, 10, 1.0).shade[1];
        let s = column_instance(&snow, [0.5, 64.0, 0.5], &offsets, 10, 1.0).shade[1];
        assert!(
            (s / r - SNOW_MAX_ALPHA / RAIN_MAX_ALPHA).abs() < 1e-4,
            "snow/rain alpha must be 0.8, got {}",
            s / r
        );
    }

    #[test]
    fn the_quad_v_axis_grows_downward_with_world_height() {
        let offsets = column_offset_table();
        let c = WeatherColumn {
            x: 0,
            z: 0,
            bottom_y: 60,
            top_y: 80,
            u_offset: 0.0,
            v_offset: 0.0,
            light: 1.0,
            kind: Precipitation::Rain,
        };
        let i = column_instance(&c, [0.5, 64.0, 0.5], &offsets, 10, 1.0);
        let v0 = i.axis[3];
        let v1 = i.shade[0];
        assert!(
            v1 > v0,
            "the top of the column must take the larger V (bottom_y*0.25 vs \
             top_y*0.25, WeatherEffectRenderer.java:214-215): v0={v0} v1={v1}"
        );
        assert!((v1 - v0 - 5.0).abs() < 1e-5, "20 blocks * 0.25 = 5 tiles");
    }

    #[test]
    fn the_rain_ambience_never_fires_on_the_first_eligible_tick() {
        let mut rainy = WeatherState::clear();
        rainy.apply_rain_level(1.0);
        let mut a = RainAmbience::default();
        // Counter starts at 0, so `rain_roll >= 0` is true for every roll in
        // 0..3 — vanilla's post-increment can never fire immediately.
        for roll in 0..3 {
            assert!(
                a.tick(&rainy, Some([0, 64, 0]), 64.0, false, roll).is_none(),
                "roll {roll} fired on the first tick"
            );
        }
        // …and the control that it fires at all once the counter is warm.
        let mut b = RainAmbience::default();
        let mut fired = 0;
        for _ in 0..40 {
            if b.tick(&rainy, Some([0, 64, 0]), 64.0, false, 0).is_some() {
                fired += 1;
            }
        }
        assert!(
            fired >= 15,
            "with a roll of 0 the cadence should fire on most ticks after the \
             first; fired {fired} of 40"
        );
    }

    #[test]
    fn the_rain_ambience_is_silent_without_rain_or_a_landing_point() {
        let clear = WeatherState::clear();
        let mut a = RainAmbience::default();
        for _ in 0..50 {
            assert!(a.tick(&clear, Some([0, 64, 0]), 64.0, false, 0).is_none());
        }
        let mut rainy = WeatherState::clear();
        rainy.apply_rain_level(1.0);
        let mut b = RainAmbience::default();
        for _ in 0..50 {
            assert!(
                b.tick(&rainy, None, 64.0, false, 0).is_none(),
                "no landing point means nothing nearby is being rained on"
            );
        }
        // Control for both absences: rain plus a landing point does fire.
        let mut c = RainAmbience::default();
        assert!(
            (0..50).any(|_| c.tick(&rainy, Some([0, 64, 0]), 64.0, false, 0).is_some()),
            "the detector must be able to fire, or the two loops above prove nothing"
        );
    }

    #[test]
    fn rain_landing_above_a_covered_ear_picks_the_muffled_variant() {
        let mut rainy = WeatherState::clear();
        rainy.apply_rain_level(1.0);
        let mut a = RainAmbience::default();
        let mut got = None;
        for _ in 0..40 {
            if let Some(s) = a.tick(&rainy, Some([0, 80, 0]), 64.0, true, 0) {
                got = Some(s);
                break;
            }
        }
        assert_eq!(got, Some(RAIN_SOUND_ABOVE));
        // Both conjuncts must be required: landing high but no roof is the near
        // variant, and a roof with rain landing at ear level is too.
        let mut b = RainAmbience::default();
        let mut no_roof = None;
        for _ in 0..40 {
            if let Some(s) = b.tick(&rainy, Some([0, 80, 0]), 64.0, false, 0) {
                no_roof = Some(s);
                break;
            }
        }
        assert_eq!(no_roof, Some(RAIN_SOUND_NEAR));
        let mut c = RainAmbience::default();
        let mut low = None;
        for _ in 0..40 {
            if let Some(s) = c.tick(&rainy, Some([0, 64, 0]), 64.0, true, 0) {
                low = Some(s);
                break;
            }
        }
        assert_eq!(low, Some(RAIN_SOUND_NEAR));
    }
}
