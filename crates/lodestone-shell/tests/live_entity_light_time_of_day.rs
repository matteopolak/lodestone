//! Live diagnosis of "mobs are still super bright, even at night".
//!
//! `53850ce`/`52f109f` made entities sample world light and installed the
//! sampler, and the sampler demonstrably works — `entity_light_at` returns a
//! real byte inside a streamed chunk. The player still reports full-bright mobs
//! at night. This test locates *why*, at the one value the entity shader
//! actually consumes.
//!
//! **The claim under test**: a server's sky-light array is time-*invariant*. It
//! encodes how much sky reaches a block, not how bright the sky currently is.
//! Vanilla darkens at night purely client-side, in `LightTexture`, by scaling
//! the sky contribution by `Level.getSkyDarken(partialTick)`. If that is true,
//! then `entity.wgsl`'s light term is **1.0 at midnight exactly as at noon**, and
//! no amount of correct sampling or correct shader plumbing can darken a mob —
//! the input never changes. (That was true of the retired `0.2 + 0.8 * max(sky,
//! block)` ramp and is equally true of vanilla's `lightmap.fsh` curve that
//! replaced it: both reach exactly `1.0` at full light.)
//!
//! This is an assertion of an *absence* (the byte does not change), so it needs
//! a control proving the detector would have fired. The control is the server's
//! own clock, read back through the same shared handle the sampler uses:
//! `world_time().1` must move from noon to midnight across the two samples. If
//! the RCON command had not landed, or the client had not observed the new time,
//! the control fails and the "unchanged" half means nothing.
//!
//! ```text
//! cargo test -p lodestone-shell --features live \
//!     --test live_entity_light_time_of_day -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::net::{NetClient, NetUpdate, entity_light_at};
use lodestone_model::math::Vec3;
use lodestone_testsupport::RconClient;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25565;
const RCON: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";
/// Vanilla 26.2, resolved through the registry by the `live` feature.
const PROTOCOL: i32 = 776;

/// `entity.wgsl`'s `lightmap_term`, in Rust, from a packed `sky << 4 | block`
/// byte, **with no sky darkening** — that omission is the point of this file, so
/// what it prints is what the shader would compute if the server's byte were the
/// only input.
///
/// Written out from `assets/minecraft/shaders/core/lightmap.fsh` rather than
/// calling `lodestone_render::light`, so this stays an independent statement of
/// what the shader should do: `get_brightness(level) = level / (4 - 3 * level)`,
/// then `mix(c, notGamma(c), 0.5)` with `notGamma(c) == 1 - (1 - c)^4` for a grey
/// value. Both terms are exactly `1.0` at full light, which is why the assertion
/// below is unchanged from when this was `0.2 + 0.8 * max(sky, block)`.
fn light_term(packed: u8) -> f32 {
    let brightness = |level: f32| level / (4.0 - 3.0 * level);
    let sky = brightness(f32::from((packed >> 4) & 15) / 15.0);
    let block = brightness(f32::from(packed & 15) / 15.0);
    let c = sky.max(block).clamp(0.0, 1.0);
    c + ((1.0 - (1.0 - c).powi(4)) - c) * 0.5
}

/// Wait until the client has logged in and holds streamed columns plus a server
/// position, or panic with a repair hint (a missing precondition is a failure,
/// never a skip).
fn join(net: &NetClient) -> Vec3 {
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut logged_in = false;
    let mut last_err: Option<String> = None;
    while Instant::now() < deadline {
        for u in net.poll() {
            match u {
                NetUpdate::LoggedIn { .. } => logged_in = true,
                NetUpdate::Error(e) => last_err = Some(e),
                NetUpdate::Disconnected(r) => {
                    last_err = Some(format!("disconnected: {}", r.to_plain_string()))
                }
                _ => {}
            }
        }
        if logged_in && net.loaded_chunks().len() >= 4 && net.server_position().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        logged_in,
        "never logged in to {HOST}:{PORT} within 45s (last event: {last_err:?}). \
         Fix: start the oracle and run with `--features live`."
    );
    net.server_position().expect(
        "logged in but the server never sent a position; without one there is no world point \
         to sample light at",
    )
}

/// The server's `time_of_day`, read through the very handle the entity light
/// sampler is installed over. Returns `None` until the handle resolves.
fn time_of_day(net: &NetClient) -> Option<i64> {
    net.shared_handle().get().map(|h| h.world_time().1)
}

/// Find a block position near the player whose **sky** nibble is 15, so the
/// sample is genuinely sky-lit and the test is about the sky term rather than
/// about a torch. Scans upward from the feet, which is where a surface spawn's
/// open air is.
fn open_sky_position(net: &NetClient, feet: Vec3) -> ([i32; 3], u8) {
    let x = feet.x.floor() as i32;
    let z = feet.z.floor() as i32;
    let base = feet.y.floor() as i32;
    let mut seen = Vec::new();
    for dy in 0..24 {
        let y = base + dy;
        // `SkyDefault::Full` because the oracle this gate drives is the overworld.
        // It is not a formality: with `None` the scan would read every section
        // above the top of the lit column as sky 0 and never find its `== 15`.
        if let Some(packed) =
            entity_light_at(&net.shared_handle(), x, y, z, lodestone_render::SkyDefault::Full)
        {
            seen.push((y, packed));
            if (packed >> 4) & 15 == 15 {
                return ([x, y, z], packed);
            }
        }
    }
    panic!(
        "no sky-15 block found in the 24 blocks above the player's feet at ({x}, {base}, {z}); \
         samples were {seen:?}. Either the player is underground or the light seam is broken \
         (an all-`None` scan means `entity_light_at` found no resident light data at all)."
    );
}

/// Poll the shared handle until the server's reported `time_of_day` lands in
/// `want`, so the two light samples are taken at genuinely different times of
/// day rather than at whatever the clock happened to be.
fn await_time(net: &NetClient, want: std::ops::RangeInclusive<i64>) -> i64 {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last = None;
    while Instant::now() < deadline {
        // Keep draining, or the net thread's bounded update channel stalls and
        // the client stops applying the server's time updates.
        let _ = net.poll();
        if let Some(t) = time_of_day(net) {
            // Vanilla's `time_of_day` counts up without wrapping at 24000 once
            // `doDaylightCycle` has run for a while; compare within the day.
            let day = t.rem_euclid(24_000);
            last = Some(day);
            if want.contains(&day) {
                return day;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "server time never reached {want:?} within 20s (last observed {last:?}); the RCON \
         `time set` did not take effect, so the control for this test cannot fire"
    );
}

#[test]
#[ignore = "requires the live oracle on 127.0.0.1:25565 with RCON on :25566, and `--features live`"]
fn the_servers_sky_light_byte_is_identical_at_noon_and_midnight() {
    let mut rcon = RconClient::connect(RCON, RCON_PASSWORD).expect(
        "connect to RCON on 127.0.0.1:25566 with password `lodestone`. Fix: start the oracle \
         with RCON enabled.",
    );
    // Freeze the clock so `time set` sticks long enough to sample, and clear
    // weather: rain and thunder are separate multipliers on vanilla's sky
    // darken, and leaving them free would make this measurement ambiguous.
    rcon.cmd("gamerule doDaylightCycle false");
    rcon.cmd("weather clear");

    let net = NetClient::connect(HOST.into(), PORT, PROTOCOL, None);
    let feet = join(&net);

    rcon.cmd("time set noon");
    let noon_clock = await_time(&net, 5_500..=6_500);
    let (pos, noon_packed) = open_sky_position(&net, feet);
    let [x, y, z] = pos;

    rcon.cmd("time set midnight");
    let midnight_clock = await_time(&net, 17_500..=18_500);
    // Give the server every chance to resend light for this column if it were
    // ever going to: drain updates for a further second before re-sampling.
    let settle = Instant::now() + Duration::from_secs(1);
    while Instant::now() < settle {
        let _ = net.poll();
        std::thread::sleep(Duration::from_millis(50));
    }
    let midnight_packed =
        entity_light_at(&net.shared_handle(), x, y, z, lodestone_render::SkyDefault::Full)
            .expect("the column that was resident at noon must still be resident at midnight");

    let noon_term = light_term(noon_packed);
    let midnight_term = light_term(midnight_packed);

    eprintln!(
        "sample ({x}, {y}, {z})\n  \
         noon     clock={noon_clock:>5}  packed=0x{noon_packed:02X} \
         sky={:>2} block={:>2} light_term={noon_term:.3}\n  \
         midnight clock={midnight_clock:>5}  packed=0x{midnight_packed:02X} \
         sky={:>2} block={:>2} light_term={midnight_term:.3}",
        (noon_packed >> 4) & 15,
        noon_packed & 15,
        (midnight_packed >> 4) & 15,
        midnight_packed & 15,
    );

    // The control: the clock really did move half a day between the two
    // samples. Without this, "the byte did not change" is indistinguishable
    // from "nothing happened at all".
    assert!(
        (midnight_clock - noon_clock).abs() > 10_000,
        "control did not fire: server clock went {noon_clock} -> {midnight_clock}, less than \
         half a day apart. The two samples were not taken at different times of day, so the \
         unchanged light byte proves nothing."
    );

    assert_eq!(
        noon_packed, midnight_packed,
        "the server's packed sky/block light at ({x}, {y}, {z}) changed between noon and \
         midnight (0x{noon_packed:02X} -> 0x{midnight_packed:02X}). If this ever fails, the \
         premise of the client-side sky-darken term is wrong and it should be removed."
    );
    assert!(
        (midnight_term - 1.0).abs() < 1e-6,
        "expected the entity shader's light_term to be a full 1.0 at midnight (that is the \
         defect), got {midnight_term}"
    );

    eprintln!(
        "DIAGNOSIS: the entity light byte is time-invariant, so `light_term` is \
         {noon_term:.3} at noon and {midnight_term:.3} at midnight. Sampling and shader \
         plumbing are both correct; the missing term is client-side sky darkening."
    );
}
