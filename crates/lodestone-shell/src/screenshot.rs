//! `key.screenshot`: read the window's own swapchain texture back
//! and write it as a PNG. The one keybind in this client that ends at a file
//! rather than at a packet — see `docs/keybindings.md`'s "Screenshot" section
//! for the vanilla parity table and the two deliberate divergences.
//!
//! The read-back is `HeadlessTarget::read_texels`' idiom
//! (`copy_texture_to_buffer` → `map_async(MapMode::Read)`) pointed at a
//! [`wgpu::Texture`] the caller supplies, because a swapchain texture is
//! per-frame and cannot be owned here. Two things the headless path does not
//! have to care about and this one does:
//!
//! * **The row stride is padded** to [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`],
//!   so the rows have to be re-packed on the way out.
//! * **The swapchain is usually `Bgra8*` on Metal and Vulkan**, and the `png`
//!   crate writes RGBA only. [`to_rgba8`] does the swizzle, keyed off the real
//!   [`wgpu::TextureFormat`] rather than assumed.
//!
//! **Call this immediately before `AcquiredFrame::present`, never straight
//! after `acquire`** — a swapchain image has no defined content until a pass
//! has written into it, so an early capture reads garbage rather than a frame.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The directory screenshots are written to, created on demand.
///
/// **Deliberately two bodies rather than one body with a `cfg!(test)` branch.**
/// CLAUDE.md's "OS-level side effect" hazard is a real incident in this repo: a
/// unit test opening a browser on every `cargo test`. A runtime `cfg!(test)`
/// check would be a *silent skip* — nothing fails if it is deleted. A `#[cfg]`
/// fork is assertable, and [`tests::the_test_build_writes_into_a_temp_dir`]
/// asserts it, so removing the fork reddens a test instead of quietly pointing
/// the suite at a player's real `screenshots/`.
#[cfg(not(test))]
#[must_use]
pub fn screenshot_dir() -> PathBuf {
    // Relative to the process's working directory: this client has no separate
    // "game directory" concept yet, so there is nothing else to be relative to.
    PathBuf::from("screenshots")
}

#[cfg(test)]
#[must_use]
pub fn screenshot_dir() -> PathBuf {
    std::env::temp_dir().join("lodestone-screenshot-tests")
}

/// Vanilla's filename scheme: `yyyy-MM-dd_HH.mm.ss.png`
/// (`Screenshot.getFile`, `Screenshot.java:136-148`), with `_2`, `_3`, … on a
/// same-second collision so an existing file is never overwritten.
///
/// **In UTC, not local time** — a named divergence from
/// `Util.getFilenameFormattedDateTime()`, which uses the local clock. Worth
/// revisiting if a calendar crate ever lands in this workspace for another
/// reason; hand-rolling one timezone database for one filename is not worth it.
#[must_use]
pub fn timestamp_name(now: SystemTime) -> String {
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}_{hh:02}.{mm:02}.{ss:02}")
}

/// Howard Hinnant's `civil_from_days`: days-since-1970-01-01 to `(y, m, d)`.
///
/// Ported rather than pulled in because it is fifteen lines of integer
/// arithmetic with no timezone or leap-second component, and the alternative is
/// a calendar crate in the shell's dependency graph for one filename.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1); // [1, 12]
    let y = yoe as i64 + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// The first free `<dir>/<base>.png`, `<dir>/<base>_2.png`, … .
///
/// Vanilla's loop, and it has vanilla's race: two captures in the same second
/// from two processes can both pick the same free name. Single-process here, so
/// the only writer is the render thread.
#[must_use]
pub fn unused_path(dir: &Path, base: &str) -> PathBuf {
    let first = dir.join(format!("{base}.png"));
    if !first.exists() {
        return first;
    }
    for n in 2u32.. {
        let candidate = dir.join(format!("{base}_{n}.png"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

/// Re-pack a padded read-back buffer into tight RGBA8 rows, swizzling BGRA if
/// that is what the swapchain handed us.
///
/// `padded` is the buffer's real row stride in bytes; `width`/`height` are in
/// texels. Returns `None` for a format this cannot express as 8-bit RGBA.
#[must_use]
pub fn to_rgba8(
    padded_rows: &[u8],
    padded: u32,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> Option<Vec<u8>> {
    // Keyed off the real format rather than assumed: a Metal or Vulkan
    // swapchain is typically `Bgra8UnormSrgb`, but an X11/GL one can be RGBA,
    // and guessing wrong swaps every red and blue in the file.
    let swizzle = match format {
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => true,
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => false,
        _ => return None,
    };
    let row_bytes = (width as usize) * 4;
    let mut out = Vec::with_capacity(row_bytes * height as usize);
    for row in 0..height as usize {
        let start = row * padded as usize;
        let src = padded_rows.get(start..start + row_bytes)?;
        if swizzle {
            for px in src.chunks_exact(4) {
                // The alpha a swapchain reports is not meaningful (the surface
                // is opaque), and vanilla's own PNG is opaque too. Forcing 255
                // avoids a fully-transparent screenshot on a backend that hands
                // back zeroed alpha.
                out.extend_from_slice(&[px[2], px[1], px[0], 255]);
            }
        } else {
            for px in src.chunks_exact(4) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
    }
    Some(out)
}

/// Encode tight RGBA8 rows as a PNG byte stream.
///
/// # Errors
/// Propagates any `png` encoding failure.
pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(out)
}

/// Copy `texture` back to the CPU and write it to [`screenshot_dir`] as a PNG.
///
/// Returns the path written. Errors are logged and swallowed by the caller —
/// a failed screenshot must never take the frame loop down.
///
/// # Errors
/// Any I/O failure creating the directory or writing the file, or an
/// unrepresentable texture format.
pub fn capture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    now: SystemTime,
) -> std::io::Result<PathBuf> {
    let width = texture.width();
    let height = texture.height();
    let format = texture.format();

    const BPP: u32 = 4;
    let unpadded = width * BPP;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;

    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("lodestone-screenshot readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("lodestone-screenshot encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());

    let rgba = {
        let view = readback.slice(..).get_mapped_range();
        let view = view.map_err(|e| std::io::Error::other(format!("map screenshot buffer: {e}")))?;
        to_rgba8(&view, padded, width, height, format).ok_or_else(|| {
            std::io::Error::other(format!("screenshot: unsupported surface format {format:?}"))
        })?
    };
    readback.unmap();

    let png_bytes = encode_png(&rgba, width, height)
        .map_err(|e| std::io::Error::other(format!("encode screenshot: {e}")))?;

    let dir = screenshot_dir();
    std::fs::create_dir_all(&dir)?;
    let path = unused_path(&dir, &timestamp_name(now));
    std::fs::write(&path, png_bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The `#[cfg(test)]` fork is the *point* of `screenshot_dir`, so it gets a
    /// test rather than a comment: deleting the fork must redden something.
    /// Without this, reverting to the single production body would silently
    /// point every `cargo test -p lodestone-shell` at a real `screenshots/`
    /// directory in whatever the working directory happens to be.
    #[test]
    fn the_test_build_writes_into_a_temp_dir() {
        let dir = screenshot_dir();
        assert!(
            dir.starts_with(std::env::temp_dir()),
            "the #[cfg(test)] body of screenshot_dir must be the one compiled here, \
             or the suite writes into a player's real screenshots directory: {dir:?}"
        );
        assert_ne!(
            dir,
            PathBuf::from("screenshots"),
            "this is the production body, which must never be compiled into a test"
        );
    }

    /// Expected values from outside our own code: hand-checked civil dates,
    /// including the two cases a naive `days / 365` gets wrong.
    #[test]
    fn the_timestamp_matches_vanillas_pattern_on_hand_checked_dates() {
        let at = |s: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(s);
        assert_eq!(timestamp_name(at(0)), "1970-01-01_00.00.00");
        // 2000-02-29, the leap day a `%100`-only rule drops.
        assert_eq!(timestamp_name(at(951_782_400)), "2000-02-29_00.00.00");
        // 2100-03-01: 2100 is NOT a leap year (divisible by 100, not by 400).
        assert_eq!(timestamp_name(at(4_107_542_400)), "2100-03-01_00.00.00");
        // A time-of-day with all three fields distinct and two-digit padded.
        assert_eq!(timestamp_name(at(1_700_000_000)), "2023-11-14_22.13.20");
    }

    #[test]
    fn a_same_second_collision_suffixes_rather_than_overwrites() {
        let dir = screenshot_dir().join("collision-7q3n");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let base = "2026-08-05_12.00.00";

        assert_eq!(unused_path(&dir, base), dir.join(format!("{base}.png")));
        std::fs::write(dir.join(format!("{base}.png")), b"x").expect("write");
        assert_eq!(unused_path(&dir, base), dir.join(format!("{base}_2.png")));
        std::fs::write(dir.join(format!("{base}_2.png")), b"x").expect("write");
        assert_eq!(unused_path(&dir, base), dir.join(format!("{base}_3.png")));

        // The control: the scheme must not be "always suffix" either — a fresh
        // base still takes the unsuffixed name.
        assert_eq!(
            unused_path(&dir, "2026-08-05_12.00.01"),
            dir.join("2026-08-05_12.00.01.png")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The swizzle is the half a wrong-format assumption gets silently wrong,
    /// so it is asserted per format with a pixel whose channels are all
    /// distinct — a symmetric grey would pass under either hypothesis.
    #[test]
    fn bgra_is_swizzled_and_rgba_is_not() {
        // One 2x1 row, padded to 256 bytes as a real read-back would be.
        let mut padded = vec![0u8; 256];
        padded[0..8].copy_from_slice(&[10, 20, 30, 0, 40, 50, 60, 0]);

        let bgra = to_rgba8(&padded, 256, 2, 1, wgpu::TextureFormat::Bgra8UnormSrgb)
            .expect("bgra is representable");
        assert_eq!(
            bgra,
            vec![30, 20, 10, 255, 60, 50, 40, 255],
            "B and R must be exchanged, G held, alpha forced opaque"
        );

        let rgba = to_rgba8(&padded, 256, 2, 1, wgpu::TextureFormat::Rgba8UnormSrgb)
            .expect("rgba is representable");
        assert_eq!(
            rgba,
            vec![10, 20, 30, 255, 40, 50, 60, 255],
            "an RGBA swapchain must NOT be swizzled"
        );
        assert_ne!(bgra, rgba, "the two formats must not decode identically");

        assert!(
            to_rgba8(&padded, 256, 2, 1, wgpu::TextureFormat::Rgba16Float).is_none(),
            "an unrepresentable format must be refused, not silently reinterpreted"
        );
    }

    /// Row padding is dropped, not carried into the file. A gate that only
    /// checked the first row would pass with the stride bug intact.
    #[test]
    fn the_padded_row_stride_is_stripped_on_every_row() {
        // 2 texels wide (8 real bytes), 3 rows, 256-byte stride.
        let mut padded = vec![0xFFu8; 256 * 3];
        for row in 0..3usize {
            let v = (row as u8 + 1) * 3;
            padded[row * 256..row * 256 + 8]
                .copy_from_slice(&[v, v, v, 0, v + 1, v + 1, v + 1, 0]);
        }
        let out = to_rgba8(&padded, 256, 2, 3, wgpu::TextureFormat::Rgba8Unorm).expect("rgba");
        assert_eq!(out.len(), 2 * 3 * 4, "the 0xFF padding must not reach the file");
        assert_eq!(&out[0..4], &[3, 3, 3, 255]);
        assert_eq!(&out[8..12], &[6, 6, 6, 255], "row 1 must start at stride 256");
        assert_eq!(&out[16..20], &[9, 9, 9, 255], "row 2 must start at stride 512");
    }

    /// The encoder writes a real PNG, not a buffer only our own reader accepts:
    /// the signature and the IHDR dimensions come from the spec, not from us.
    #[test]
    fn the_encoded_bytes_are_a_real_png_of_the_right_size() {
        let rgba = vec![0u8; 4 * 4 * 4];
        let bytes = encode_png(&rgba, 4, 4).expect("encode");
        assert_eq!(
            &bytes[0..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
            "PNG signature, RFC 2083 §3.1"
        );
        assert_eq!(&bytes[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]), 4);
        assert_eq!(u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]), 4);
    }
}
