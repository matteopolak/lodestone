//! How many times `extract_columns` interrogates the world, per frame — each
//! query is frame-invariant per column, so any count above one per column is
//! pure waste.
//!
//! The shell's `WeatherProbe` impl reaches the client-owned world, and every
//! `precipitation` call used to cost three lock acquisitions. The frequency half
//! of that cost belongs to *this* crate: `extract_columns` decides how many times
//! the probe is asked anything at all, and it asked `column_top` **twice per
//! column for the same value**.
//!
//! Counting the trait's own invocations is a measurement of the subject, not of a
//! double: `WeatherProbe` exists to be implemented by the shell, so the thing
//! under test is how many times the extraction drives it. The expected numbers
//! come from [`lodestone_render::DEFAULT_WEATHER_RADIUS`] and the `2r + 1` square
//! `extract_columns` documents, i.e. from outside the code under test.
//!
//! **World species**: a clear-weather frame cannot exercise any of this —
//! `extract_columns` returns empty on `!any_precipitation()` before touching the
//! probe. `rainy()` below is the load-bearing part of the fixture, and
//! `a_clear_frame_asks_the_probe_nothing_at_all` is the control that says so.

use std::cell::Cell;

use lodestone_render::{
    DEFAULT_WEATHER_RADIUS, Precipitation, WeatherProbe, WeatherState, extract_columns,
};

/// Columns in the square `extract_columns` walks, derived from the same radius
/// the production caller passes rather than restated as a literal.
const COLUMNS: u32 = ((2 * DEFAULT_WEATHER_RADIUS + 1) * (2 * DEFAULT_WEATHER_RADIUS + 1)) as u32;

/// A probe that answers from pure arithmetic and counts what it was asked.
///
/// `top` is deliberately a function of `(x, z)`: a hoist that lifted the
/// `column_top` call *out* of the per-column loop would keep the call count
/// correct and silently give every column the same span, so the spans are
/// asserted against the same expression below.
struct CountingProbe {
    kind: Precipitation,
    top: Option<i32>,
    column_top_calls: Cell<u32>,
    precipitation_calls: Cell<u32>,
    light_calls: Cell<u32>,
}

impl CountingProbe {
    fn new(kind: Precipitation, top: Option<i32>) -> Self {
        Self {
            kind,
            top,
            column_top_calls: Cell::new(0),
            precipitation_calls: Cell::new(0),
            light_calls: Cell::new(0),
        }
    }
}

impl WeatherProbe for CountingProbe {
    fn column_top(&self, x: i32, z: i32) -> Option<i32> {
        self.column_top_calls.set(self.column_top_calls.get() + 1);
        // Per-column, so a loop-invariant hoist is visible in the output.
        self.top.map(|t| t + (x.rem_euclid(3)) - (z.rem_euclid(2)))
    }

    fn precipitation(&self, _x: i32, _y: i32, _z: i32) -> Precipitation {
        self.precipitation_calls
            .set(self.precipitation_calls.get() + 1);
        self.kind
    }

    fn light(&self, _x: i32, _y: i32, _z: i32) -> f32 {
        self.light_calls.set(self.light_calls.get() + 1);
        1.0
    }
}

fn rainy() -> WeatherState {
    let mut w = WeatherState::clear();
    w.apply_rain_level(1.0);
    w
}

#[test]
fn one_column_top_query_per_column_not_two() {
    let probe = CountingProbe::new(Precipitation::Rain, None);
    let columns = extract_columns(
        &rainy(),
        DEFAULT_WEATHER_RADIUS,
        0,
        0.0,
        [0.5, 64.5, 0.5],
        &probe,
    );

    // The two competing hypotheses, both computed from `COLUMNS`: `2 * COLUMNS`
    // if `column_top` is still called once for the span and again for the light
    // sample, `COLUMNS` if the value is resolved once and reused.
    assert_eq!(
        probe.column_top_calls.get(),
        COLUMNS,
        "expected one `column_top` query per column ({COLUMNS}); the pre-fix \
         implementation asked twice ({}), and anything else means the loop shape \
         changed",
        2 * COLUMNS
    );
    assert_eq!(probe.precipitation_calls.get(), COLUMNS);
    assert_eq!(probe.light_calls.get(), COLUMNS);
    assert_eq!(
        columns.len(),
        COLUMNS as usize,
        "every column rains, so every column is emitted — a smaller count means \
         the fixture stopped exercising the path it exists to measure"
    );
}

#[test]
fn a_column_that_does_not_rain_is_never_light_sampled() {
    // The control for the counter itself: `Precipitation::None` short-circuits
    // *between* the span query and the light query, so the three counts must
    // separate. If they did not, the counters would be measuring nothing.
    let probe = CountingProbe::new(Precipitation::None, None);
    let columns = extract_columns(
        &rainy(),
        DEFAULT_WEATHER_RADIUS,
        0,
        0.0,
        [0.5, 64.5, 0.5],
        &probe,
    );
    assert_eq!(probe.column_top_calls.get(), COLUMNS);
    assert_eq!(probe.precipitation_calls.get(), COLUMNS);
    assert_eq!(probe.light_calls.get(), 0);
    assert!(columns.is_empty());
}

#[test]
fn a_clear_frame_asks_the_probe_nothing_at_all() {
    // The world-species control: with no precipitation there is nothing to
    // measure, so a gate built on a clear `WeatherState` would pass whatever the
    // implementation did.
    let probe = CountingProbe::new(Precipitation::Rain, Some(70));
    let columns = extract_columns(
        &WeatherState::clear(),
        DEFAULT_WEATHER_RADIUS,
        0,
        0.0,
        [0.5, 64.5, 0.5],
        &probe,
    );
    assert_eq!(probe.column_top_calls.get(), 0);
    assert_eq!(probe.precipitation_calls.get(), 0);
    assert_eq!(probe.light_calls.get(), 0);
    assert!(columns.is_empty());
}

#[test]
fn the_reused_column_top_is_still_this_columns_own_height() {
    // A per-column `top`, so the reuse has to be per-column too. Vanilla's span
    // is `(max(cam_y - r, terrain), max(cam_y + r, terrain))` and its light
    // sample is at `max(cam_y, terrain)` — restated here from
    // `WeatherEffectRenderer`'s own decompiled source's shape rather than from the
    // implementation, so a hoist out of the loop lands on a different answer.
    let radius = DEFAULT_WEATHER_RADIUS;
    let cam = [0.5_f64, 64.5_f64, 0.5_f64];
    let cam_x = cam[0].floor() as i32;
    let cam_y = cam[1].floor() as i32;
    let cam_z = cam[2].floor() as i32;
    let base = 70;

    let probe = CountingProbe::new(Precipitation::Rain, Some(base));
    let columns = extract_columns(&rainy(), radius, 0, 0.0, cam, &probe);

    assert_eq!(columns.len(), COLUMNS as usize);
    let mut checked = 0_u32;
    for c in &columns {
        let terrain = base + c.x.rem_euclid(3) - c.z.rem_euclid(2);
        assert_eq!(
            (c.bottom_y, c.top_y),
            ((cam_y - radius).max(terrain), (cam_y + radius).max(terrain)),
            "column ({}, {}) got the wrong span — a `column_top` hoisted out of \
             the per-column loop looks exactly like this",
            c.x,
            c.z
        );
        checked += 1;
    }
    assert_eq!(checked, COLUMNS);
    // And the fixture really does vary by column, or the assertion above is
    // satisfied by one constant span.
    let spans: std::collections::BTreeSet<(i32, i32)> =
        columns.iter().map(|c| (c.bottom_y, c.top_y)).collect();
    assert!(
        spans.len() >= 2,
        "the fixture must produce more than one distinct span, or it cannot see \
         a loop-invariant hoist"
    );
    let _ = (cam_x, cam_z);
}
