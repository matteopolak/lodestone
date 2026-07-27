//! Adversarial / non-tidy packs.
//!
//! The renderer's real NPOT mip panic came from a suite where every test used a
//! tidy 16x16 single-sprite atlas — 100% green over a function that died on the
//! first realistic input. These tests deliberately feed awkward geometry and
//! assert against *authorities not computed here*: the input pixels themselves
//! (content round-trip), vanilla's own `SpriteLoader` mip-cap formula (derived
//! independently from the decompiled source, not from `atlas.rs`), and the
//! standalone mip generator. Animation schedules are chosen so a wrong
//! (equal-frametime) implementation is forced to diverge, not merely allowed to.

use lodestone_assets::{
    AnimationFrame, AnimationMeta, AtlasBuilder, Image, MipStrategy, ResourceLocation, TextureMeta,
    generate_mip_levels,
};

fn img(w: u32, h: u32, c: [u8; 4]) -> Image {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..w * h {
        rgba.extend_from_slice(&c);
    }
    Image {
        width: w,
        height: h,
        rgba,
    }
}

/// A deterministic gradient image, so a misplaced or transposed blit is caught
/// (a solid colour would round-trip even if rows/cols were swapped).
fn gradient(w: u32, h: u32) -> Image {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[(x * 7) as u8, (y * 11) as u8, (x ^ y) as u8, 255]);
        }
    }
    Image {
        width: w,
        height: h,
        rgba,
    }
}

fn loc(s: &str) -> ResourceLocation {
    ResourceLocation::parse(s).unwrap()
}

fn anim(frames: Vec<AnimationFrame>, frametime: u32, interp: bool) -> TextureMeta {
    TextureMeta {
        animation: Some(AnimationMeta {
            frametime,
            interpolate: interp,
            frame_width: None,
            frame_height: None,
            frames,
        }),
        other_sections: vec![],
    }
}

/// Reads the atlas base level back at a sprite's pixel rect. The authority for
/// "is the sprite placed correctly" is the *input image*, not any UV I compute.
fn read_region(rgba: &[u8], atlas_w: u32, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
    let aw = atlas_w as usize;
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h as usize {
        let start = ((y as usize + row) * aw + x as usize) * 4;
        out.extend_from_slice(&rgba[start..start + w as usize * 4]);
    }
    out
}

/// Vanilla `SpriteLoader.stitch` mip-level rule, transcribed straight from the
/// decompiled source and computed only from sprite dimensions — an authority
/// independent of `atlas.rs`:
///   lowestOneBit(x) = 1 << trailing_zeros(x)
///   minSize = min over sprites of min(lowestOneBit(w), lowestOneBit(h))
///   mipLevel = min(requested, log2(minSize))
fn vanilla_mip_levels(requested: u32, dims: &[(u32, u32)]) -> u32 {
    let mut lowest = 1u32 << requested;
    for &(w, h) in dims {
        let lob = (1u32 << w.trailing_zeros()).min(1u32 << h.trailing_zeros());
        lowest = lowest.min(lob);
    }
    lowest.trailing_zeros().min(requested)
}

#[test]
fn awkward_geometry_round_trips_and_never_panics() {
    // A pack no sane artist would ship, all in one atlas: non-square, NPOT,
    // single-pixel, a 1-wide tower, and a fully transparent tile.
    let sprites: Vec<(&str, Image)> = vec![
        ("minecraft:block/square", gradient(16, 16)),
        ("minecraft:block/wide", gradient(16, 8)),
        ("minecraft:block/npot", gradient(13, 7)),
        ("minecraft:block/pixel", gradient(1, 1)),
        ("minecraft:block/tower", gradient(1, 64)),
        ("minecraft:block/clear", img(16, 16, [0, 0, 0, 0])),
    ];
    let mut b = AtlasBuilder::new().with_mip_levels(4);
    for (name, image) in &sprites {
        b.add_texture(loc(name), image.clone(), None);
    }
    let atlas = b.build().expect("awkward pack must stitch, not panic");

    // Every sprite present, and its atlas region is byte-identical to its input.
    for (name, image) in &sprites {
        let s = atlas
            .sprite(&loc(name))
            .unwrap_or_else(|| panic!("{name} missing from atlas"));
        assert_eq!((s.width, s.height), (image.width, image.height));
        let got = read_region(&atlas.rgba, atlas.width, s.x, s.y, s.width, s.height);
        assert_eq!(
            got, image.rgba,
            "{name} placed content differs from its input"
        );
    }

    // Mip depth must match vanilla's rule: the 1x1 pixel and 13x7 both have
    // lowestOneBit 1, so the whole atlas drops to zero downsamples — exactly what
    // vanilla logs ("limits mip level ... to 0"). Prove we match, not guess.
    let dims: Vec<(u32, u32)> = sprites.iter().map(|(_, i)| (i.width, i.height)).collect();
    assert_eq!(vanilla_mip_levels(4, &dims), 0);
    assert_eq!(
        atlas.mip_count(),
        1,
        "one 1x1/NPOT sprite caps mips as vanilla does"
    );
}

#[test]
fn mip_cap_tracks_vanilla_across_dimension_mixes() {
    // Several packs whose smallest mippable dimension differs; the atlas mip
    // count must equal vanilla's independently-derived value every time.
    let cases: Vec<(u32, Vec<(u32, u32)>)> = vec![
        (4, vec![(16, 16), (32, 32)]), // all deep -> 4
        (4, vec![(16, 16), (16, 8)]),  // 8 limits -> 3
        (4, vec![(16, 16), (24, 24)]), // tz(24)=3 -> 3
        (4, vec![(16, 16), (13, 7)]),  // NPOT -> 0
        (4, vec![(64, 64), (8, 8)]),   // 8 -> 3
        (2, vec![(16, 16)]),           // requested caps below native -> 2
    ];
    for (req, dims) in cases {
        let mut b = AtlasBuilder::new().with_mip_levels(req);
        for (i, &(w, h)) in dims.iter().enumerate() {
            b.add_texture(loc(&format!("minecraft:block/s{i}")), gradient(w, h), None);
        }
        let atlas = b.build().unwrap();
        let expected = vanilla_mip_levels(req, &dims);
        assert_eq!(
            atlas.mip_count(),
            expected + 1,
            "req={req} dims={dims:?}: mip_count should be vanilla's {expected}+1"
        );
    }
}

#[test]
fn non_square_sprite_mip_content_matches_the_standalone_generator() {
    // A pack that DOES support mips (min mippable dim = 8 -> 3 levels). The
    // atlas's per-sprite mip regions must byte-match `generate_mip_levels` run on
    // the same input — cross-checking the compositor against the (separately
    // vanilla-verified) generator rather than against numbers typed here.
    let wide = gradient(16, 8);
    let square = gradient(16, 16);
    let mut b = AtlasBuilder::new().with_mip_levels(4);
    b.add_texture(loc("minecraft:block/wide"), wide.clone(), None);
    b.add_texture(loc("minecraft:block/square"), square.clone(), None);
    let atlas = b.build().unwrap();
    assert_eq!(atlas.mip_count(), 4, "min mippable dim 8 -> 3 downsamples");

    for (name, input) in [
        ("minecraft:block/wide", &wide),
        ("minecraft:block/square", &square),
    ] {
        let s = atlas.sprite(&loc(name)).unwrap();
        let chain = generate_mip_levels(input, 3, MipStrategy::Auto, 0.0);
        for level in 1..atlas.mip_count() {
            let m = atlas.mip(level).unwrap();
            let (sx, sy) = (s.x >> level, s.y >> level);
            let (sw, sh) = (s.width >> level, s.height >> level);
            let got = read_region(m.rgba, m.width, sx, sy, sw, sh);
            assert_eq!(
                got, chain[level as usize].rgba,
                "{name} atlas mip {level} != standalone generator output"
            );
        }
    }
}

#[test]
fn fully_transparent_cutout_mips_stay_invisible_matching_vanilla_bias() {
    // A fully transparent cutout: solidify() has no opaque seed, so it must not
    // divide by zero. But note the authority here is vanilla, not intuition:
    // MipmapGenerator line 84 adds `alphaCutoffBias + 0.025` to EVERY texel of a
    // scaled mip, so vanilla's own output is NOT literally alpha 0 above level 0 —
    // it is a small constant that is still far below the 0.5 alpha-test cutoff, so
    // the texel is discarded and stays invisible. Assert that faithful behavior
    // (round(0.025*255) = 6), not the tempting-but-wrong "stays exactly 0".
    let levels = generate_mip_levels(&img(16, 16, [0, 0, 0, 0]), 4, MipStrategy::Cutout, 0.0);
    assert_eq!(levels.len(), 5);

    // Base level is solidified but not coverage-scaled: it stays fully transparent.
    assert!(
        levels[0].rgba.chunks_exact(4).all(|p| p[3] == 0),
        "base level must stay fully transparent"
    );
    // Each scaled mip downsamples the already-biased previous level and adds the
    // 0.025 bias again, so alpha accumulates (6, 12, 18, 24 ...). The invariant
    // that actually matters is that it stays well below the 0.5 alpha-test cutoff,
    // so the texel is discarded and the cutout stays invisible at every distance.
    let mut prev = 0u8;
    for (level, img) in levels.iter().enumerate().skip(1) {
        let a0 = img.rgba[3];
        assert!(
            a0 > 0,
            "mip {level}: vanilla's 0.025 bias must lift alpha off zero"
        );
        assert!(
            a0 < 128,
            "mip {level}: alpha {a0} must stay below the 0.5 cutoff (invisible)"
        );
        assert!(
            img.rgba.chunks_exact(4).all(|p| p[3] == a0),
            "mip {level}: a uniform transparent input must yield a uniform alpha"
        );
        assert!(
            a0 > prev,
            "mip {level}: iterated bias must accumulate (vanilla-faithful)"
        );
        prev = a0;
    }
}

#[test]
fn animation_schedule_forces_a_wrong_impl_to_diverge() {
    // Per-frame times 5,1,2 (sum 8). An implementation that ignored them and used
    // the frametime (3) would produce cycle 9 and different frame boundaries, so
    // these assertions cannot pass by accident on the equal-time reading.
    let frames = vec![
        AnimationFrame {
            index: 0,
            time: Some(5),
        },
        AnimationFrame {
            index: 1,
            time: Some(1),
        },
        AnimationFrame {
            index: 2,
            time: Some(2),
        },
    ];
    let mut b = AtlasBuilder::new();
    b.add_texture(
        loc("minecraft:block/fire"),
        img(16, 48, [1, 2, 3, 255]),
        Some(anim(frames, 3, true)),
    );
    let atlas = b.build().unwrap();
    let s = atlas.sprite(&loc("minecraft:block/fire")).unwrap();

    assert_eq!(
        s.cycle_ticks(),
        8,
        "cycle is the sum of per-frame times, not 3*frames"
    );
    assert!(s.interpolate, "interpolate flag must survive to the sprite");

    // Slot boundaries: [0,5)->frame0, [5,6)->frame1, [6,8)->frame2.
    assert_eq!(s.frame_at_tick(0).current, 0);
    assert_eq!(s.frame_at_tick(4).current, 0);
    assert_eq!(s.frame_at_tick(5).current, 1);
    assert_eq!(s.frame_at_tick(6).current, 2);
    assert_eq!(s.frame_at_tick(7).current, 2);
    assert_eq!(s.frame_at_tick(8).current, 0, "wraps at the cycle length");
    // `next` wraps to the first frame from the last slot.
    assert_eq!(s.frame_at_tick(7).next, 0);
    // Blend rises within a multi-tick slot (frame2 spans 2 ticks: 6,7).
    let b6 = s.frame_at_tick(6).blend;
    let b7 = s.frame_at_tick(7).blend;
    assert!(b6 < b7, "blend must advance across a slot: {b6} !< {b7}");
}

#[test]
fn awkward_pack_build_is_deterministic() {
    let build = || {
        let mut b = AtlasBuilder::new().with_mip_levels(4);
        b.add_texture(loc("minecraft:block/npot"), gradient(13, 7), None);
        b.add_texture(loc("minecraft:block/wide"), gradient(16, 8), None);
        b.add_texture(loc("minecraft:block/square"), gradient(16, 16), None);
        b.build().unwrap()
    };
    let a = build();
    let c = build();
    assert_eq!((a.width, a.height), (c.width, c.height));
    assert_eq!(
        a.rgba, c.rgba,
        "awkward pack must stitch byte-for-byte reproducibly"
    );
}

#[test]
fn mip_cap_diagnostic_names_the_limiting_sprite() {
    // Vanilla logs which texture limited the whole atlas's mip level; a silent
    // cap gets misdiagnosed weeks later as "why are my textures blurry". A tidy
    // 16x16 pack that would mip to 4 levels, plus one 13x7 NPOT sprite that caps
    // the sheet to 0, must surface that fact — naming the offender.
    let mut b = AtlasBuilder::new().with_mip_levels(4);
    b.add_texture(loc("minecraft:block/tidy_a"), gradient(16, 16), None);
    b.add_texture(loc("minecraft:block/tidy_b"), gradient(32, 32), None);
    b.add_texture(loc("minecraft:block/npot"), gradient(13, 7), None);
    let atlas = b.build().unwrap();

    let cap = atlas
        .mip_cap()
        .expect("cap bit, so a diagnostic must be present");
    assert_eq!(cap.requested, 4);
    assert_eq!(
        cap.effective, 0,
        "one 13x7 caps the whole atlas to base only"
    );
    assert_eq!(
        cap.effective + 1,
        atlas.mip_count(),
        "diagnostic agrees with mip_count"
    );
    assert_eq!(
        cap.limiting_sprites,
        vec![loc("minecraft:block/npot")],
        "only the NPOT sprite is at the limiting level"
    );
}

#[test]
fn no_mip_cap_diagnostic_when_all_sprites_mip_fully() {
    // A tidy pack that reaches the requested depth must NOT report a cap: the
    // diagnostic is a warning, not noise on every atlas.
    let mut b = AtlasBuilder::new().with_mip_levels(4);
    b.add_texture(loc("minecraft:block/a"), gradient(16, 16), None);
    b.add_texture(loc("minecraft:block/b"), gradient(32, 32), None);
    let atlas = b.build().unwrap();
    assert_eq!(atlas.mip_count(), 5);
    assert!(atlas.mip_cap().is_none(), "no cap => no diagnostic");
}
