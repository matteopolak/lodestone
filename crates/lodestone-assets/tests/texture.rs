//! Tests for PNG decoding and `*.png.mcmeta` parsing.

use lodestone_assets::{Image, TextureError, TextureMeta};

/// Encodes a PNG with the given colour type/bit depth for use as decoder input.
fn encode(
    width: u32,
    height: u32,
    color: png::ColorType,
    depth: png::BitDepth,
    data: &[u8],
    palette: Option<Vec<u8>>,
    trns: Option<Vec<u8>>,
) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, width, height);
        enc.set_color(color);
        enc.set_depth(depth);
        if let Some(p) = palette {
            enc.set_palette(p);
        }
        if let Some(t) = trns {
            enc.set_trns(t);
        }
        let mut writer = enc.write_header().unwrap();
        writer.write_image_data(data).unwrap();
        writer.finish().unwrap();
    }
    buf
}

#[test]
fn decodes_rgba8() {
    let data = [
        255, 0, 0, 255, // red
        0, 255, 0, 128, // semi-transparent green
    ];
    let png = encode(
        2,
        1,
        png::ColorType::Rgba,
        png::BitDepth::Eight,
        &data,
        None,
        None,
    );
    let img = Image::decode_png(&png).unwrap();
    assert_eq!((img.width, img.height), (2, 1));
    assert_eq!(img.pixel(0, 0), [255, 0, 0, 255]);
    assert_eq!(img.pixel(1, 0), [0, 255, 0, 128]);
}

#[test]
fn decodes_rgb8_as_opaque() {
    let data = [10, 20, 30, 40, 50, 60];
    let png = encode(
        2,
        1,
        png::ColorType::Rgb,
        png::BitDepth::Eight,
        &data,
        None,
        None,
    );
    let img = Image::decode_png(&png).unwrap();
    assert_eq!(img.pixel(0, 0), [10, 20, 30, 255]);
    assert_eq!(img.pixel(1, 0), [40, 50, 60, 255]);
}

#[test]
fn decodes_grayscale8_as_opaque_gray() {
    let data = [0, 128, 255, 64];
    let png = encode(
        4,
        1,
        png::ColorType::Grayscale,
        png::BitDepth::Eight,
        &data,
        None,
        None,
    );
    let img = Image::decode_png(&png).unwrap();
    assert_eq!(img.pixel(0, 0), [0, 0, 0, 255]);
    assert_eq!(img.pixel(1, 0), [128, 128, 128, 255]);
    assert_eq!(img.pixel(2, 0), [255, 255, 255, 255]);
}

#[test]
fn decodes_grayscale_alpha() {
    let data = [200, 50, 100, 255];
    let png = encode(
        2,
        1,
        png::ColorType::GrayscaleAlpha,
        png::BitDepth::Eight,
        &data,
        None,
        None,
    );
    let img = Image::decode_png(&png).unwrap();
    assert_eq!(img.pixel(0, 0), [200, 200, 200, 50]);
    assert_eq!(img.pixel(1, 0), [100, 100, 100, 255]);
}

#[test]
fn decodes_palette_with_transparency() {
    // Two-entry palette: index 0 = red (transparent), index 1 = blue (opaque).
    let palette = vec![255, 0, 0, 0, 0, 255];
    let trns = vec![0, 255]; // alpha for index 0 and 1
    // 4-pixel indexed image at bit depth 8: 0,1,1,0
    let data = [0, 1, 1, 0];
    let png = encode(
        4,
        1,
        png::ColorType::Indexed,
        png::BitDepth::Eight,
        &data,
        Some(palette),
        Some(trns),
    );
    let img = Image::decode_png(&png).unwrap();
    assert_eq!(img.pixel(0, 0), [255, 0, 0, 0]); // transparent red
    assert_eq!(img.pixel(1, 0), [0, 0, 255, 255]); // opaque blue
    assert_eq!(img.pixel(3, 0), [255, 0, 0, 0]);
}

#[test]
fn decodes_low_bit_depth_grayscale() {
    // 1-bit grayscale, 8 pixels: 1,0,1,0,1,0,1,0 -> byte 0b10101010 = 0xAA.
    let data = [0b1010_1010];
    let png = encode(
        8,
        1,
        png::ColorType::Grayscale,
        png::BitDepth::One,
        &data,
        None,
        None,
    );
    let img = Image::decode_png(&png).unwrap();
    // Expanded to 8-bit: bit set -> 255, clear -> 0.
    assert_eq!(img.pixel(0, 0), [255, 255, 255, 255]);
    assert_eq!(img.pixel(1, 0), [0, 0, 0, 255]);
}

#[test]
fn malformed_png_is_rejected_without_panic() {
    assert!(matches!(
        Image::decode_png(b"not a png at all"),
        Err(TextureError::Decode(_))
    ));
    // Valid signature, truncated body.
    let mut png = encode(
        2,
        2,
        png::ColorType::Rgba,
        png::BitDepth::Eight,
        &[0; 16],
        None,
        None,
    );
    png.truncate(png.len() / 2);
    assert!(Image::decode_png(&png).is_err());
    // Empty input.
    assert!(Image::decode_png(&[]).is_err());
}

#[test]
fn animation_meta_bare_index_frames() {
    let json = br#"{"animation":{"frametime":2,"frames":[0,1,2,3]}}"#;
    let meta = TextureMeta::parse(json).unwrap();
    let anim = meta.animation.unwrap();
    assert_eq!(anim.frametime, 2);
    assert!(!anim.interpolate);
    assert_eq!(anim.frames.len(), 4);
    assert_eq!(anim.frames[2].index, 2);
    assert_eq!(anim.frames[2].time, None);
}

#[test]
fn animation_meta_indexed_time_frames() {
    let json = br#"{"animation":{"interpolate":true,"frametime":1,
        "frames":[{"index":0,"time":5},2,{"index":1,"time":10}]}}"#;
    let meta = TextureMeta::parse(json).unwrap();
    let anim = meta.animation.unwrap();
    assert!(anim.interpolate);
    assert_eq!(anim.frames[0].index, 0);
    assert_eq!(anim.frames[0].time, Some(5));
    assert_eq!(anim.frames[1].index, 2);
    assert_eq!(anim.frames[1].time, None);
    assert_eq!(anim.frames[2].index, 1);
    assert_eq!(anim.frames[2].time, Some(10));
}

#[test]
fn animation_meta_defaults() {
    let json = br#"{"animation":{}}"#;
    let anim = TextureMeta::parse(json).unwrap().animation.unwrap();
    assert_eq!(anim.frametime, 1);
    assert!(!anim.interpolate);
    assert!(anim.frames.is_empty());
    assert_eq!(anim.frame_width, None);
    assert_eq!(anim.frame_height, None);
}

#[test]
fn animation_meta_width_height() {
    let json = br#"{"animation":{"width":16,"height":16}}"#;
    let anim = TextureMeta::parse(json).unwrap().animation.unwrap();
    assert_eq!(anim.frame_width, Some(16));
    assert_eq!(anim.frame_height, Some(16));
}

#[test]
fn non_animation_mcmeta_parses_without_animation() {
    // gui/villager/texture sections must be a supported, non-error case.
    let json = br#"{"gui":{"scaling":{"type":"nine_slice"}},"texture":{"blur":false}}"#;
    let meta = TextureMeta::parse(json).unwrap();
    assert!(meta.animation.is_none());
    assert_eq!(
        meta.other_sections,
        vec!["gui".to_string(), "texture".to_string()]
    );
}

#[test]
fn malformed_mcmeta_is_rejected() {
    assert!(matches!(
        TextureMeta::parse(b"{ this is not json"),
        Err(TextureError::MetaMalformed(_))
    ));
    // animation present but wrong shape.
    assert!(TextureMeta::parse(br#"{"animation":[]}"#).is_err());
    assert!(TextureMeta::parse(br#"{"animation":{"frames":5}}"#).is_err());
}

/// [`Image::first_animation_frame`] crops the **top** slice, in row order —
/// discriminating input: two rows of distinct, non-repeating colours (never a
/// uniform fill, which a mis-cropped image could satisfy by accident). A
/// 2×4 strip, two 2×2 frames, cropped to `frame_height = 2` must keep frame
/// 0's own two rows and drop frame 1's, not the reverse and not a resize.
#[test]
fn first_animation_frame_keeps_the_top_frame_only() {
    // Row 0 (frame 0, row 0): red, green. Row 1 (frame 0, row 1): blue, white.
    // Row 2 (frame 1, row 0): black, yellow. Row 3 (frame 1, row 1): cyan, magenta.
    #[rustfmt::skip]
    let rgba: Vec<u8> = vec![
        255, 0, 0, 255,    0, 255, 0, 255,
        0, 0, 255, 255,    255, 255, 255, 255,
        0, 0, 0, 255,      255, 255, 0, 255,
        0, 255, 255, 255,  255, 0, 255, 255,
    ];
    let strip = Image {
        width: 2,
        height: 4,
        rgba,
    };
    let frame0 = strip.first_animation_frame(2);
    assert_eq!((frame0.width, frame0.height), (2, 2));
    assert_eq!(frame0.pixel(0, 0), [255, 0, 0, 255], "row 0 col 0, red");
    assert_eq!(frame0.pixel(1, 0), [0, 255, 0, 255], "row 0 col 1, green");
    assert_eq!(frame0.pixel(0, 1), [0, 0, 255, 255], "row 1 col 0, blue");
    assert_eq!(
        frame0.pixel(1, 1),
        [255, 255, 255, 255],
        "row 1 col 1, white"
    );
    // Frame 1's colours must not appear anywhere in the cropped result — the
    // control that would catch cropping the *bottom* half instead, or not
    // cropping at all.
    for y in 0..frame0.height {
        for x in 0..frame0.width {
            let px = frame0.pixel(x, y);
            assert_ne!(px, [0, 0, 0, 255], "frame 1's black leaked in at ({x},{y})");
            assert_ne!(
                px,
                [255, 255, 0, 255],
                "frame 1's yellow leaked in at ({x},{y})"
            );
        }
    }
}

/// A `frame_height` at or past the image's own height is a safe no-op, not a
/// panic and not a truncation — the case a static (non-animated) sheet takes
/// every time this is called with its own declared height.
#[test]
fn first_animation_frame_is_a_no_op_when_frame_height_covers_the_whole_image() {
    let img = Image {
        width: 2,
        height: 2,
        rgba: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
    };
    assert_eq!(img.first_animation_frame(2), img);
    assert_eq!(img.first_animation_frame(9), img);
    assert_eq!(img.first_animation_frame(0), img);
}
