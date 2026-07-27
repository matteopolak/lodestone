//! Tests for deterministic atlas stitching.

use lodestone_assets::{AtlasBuilder, AtlasError, Image, ResourceLocation, TextureMeta};

/// A solid-colour RGBA8 image.
fn solid(width: u32, height: u32, color: [u8; 4]) -> Image {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..width * height {
        rgba.extend_from_slice(&color);
    }
    Image {
        width,
        height,
        rgba,
    }
}

fn loc(s: &str) -> ResourceLocation {
    ResourceLocation::parse(s).unwrap()
}

#[test]
fn empty_builder_errors() {
    assert!(matches!(
        AtlasBuilder::new().build(),
        Err(AtlasError::Empty)
    ));
}

#[test]
fn single_sprite_uv_covers_its_region() {
    let mut b = AtlasBuilder::new().with_width(16);
    b.add_texture(
        loc("minecraft:block/stone"),
        solid(16, 16, [1, 2, 3, 255]),
        None,
    );
    let atlas = b.build().unwrap();
    let sprite = atlas.sprite(&loc("minecraft:block/stone")).unwrap();
    assert_eq!((sprite.x, sprite.y), (0, 0));
    assert_eq!((sprite.width, sprite.height), (16, 16));
    assert_eq!(sprite.uv_min, [0.0, 0.0]);
    assert_eq!(sprite.uv_max, [1.0, 1.0]);
    assert_eq!(sprite.frame_count, 1);
    assert!(!sprite.is_animated());
    // Pixels landed correctly.
    assert_eq!(&atlas.rgba[0..4], &[1, 2, 3, 255]);
}

#[test]
fn known_placement_uvs_are_correct() {
    // Force a 32px-wide atlas and add two 16x16 tiles; both fit on one shelf.
    let mut b = AtlasBuilder::new().with_width(32);
    b.add_texture(
        loc("minecraft:block/a"),
        solid(16, 16, [10, 0, 0, 255]),
        None,
    );
    b.add_texture(
        loc("minecraft:block/b"),
        solid(16, 16, [0, 20, 0, 255]),
        None,
    );
    let atlas = b.build().unwrap();
    assert_eq!(atlas.width, 32);
    // Height rounds up to a power of two; a single 16-tall shelf -> 16.
    assert_eq!(atlas.height, 16);

    let a = atlas.sprite(&loc("minecraft:block/a")).unwrap();
    let bb = atlas.sprite(&loc("minecraft:block/b")).unwrap();
    // Two equal-size tiles share a shelf at x=0 and x=16 (order by name).
    let xs = {
        let mut v = [a.x, bb.x];
        v.sort_unstable();
        v
    };
    assert_eq!(xs, [0, 16]);
    assert_eq!(a.y, 0);
    assert_eq!(bb.y, 0);

    // UV of a tile at x=16 spans [0.5, 1.0] horizontally.
    let right = if a.x == 16 { a } else { bb };
    assert_eq!(right.uv_min, [0.5, 0.0]);
    assert_eq!(right.uv_max, [1.0, 1.0]);
}

#[test]
fn deterministic_bytes_and_uvs_across_runs() {
    let build = || {
        let mut b = AtlasBuilder::new();
        // Insert in different orders to prove ordering is normalised.
        b.add_texture(
            loc("minecraft:block/c"),
            solid(16, 16, [3, 3, 3, 255]),
            None,
        );
        b.add_texture(
            loc("minecraft:block/a"),
            solid(32, 32, [1, 1, 1, 255]),
            None,
        );
        b.add_texture(
            loc("minecraft:block/b"),
            solid(16, 16, [2, 2, 2, 255]),
            None,
        );
        b.build().unwrap()
    };
    let first = build();

    let mut b2 = AtlasBuilder::new();
    b2.add_texture(
        loc("minecraft:block/a"),
        solid(32, 32, [1, 1, 1, 255]),
        None,
    );
    b2.add_texture(
        loc("minecraft:block/b"),
        solid(16, 16, [2, 2, 2, 255]),
        None,
    );
    b2.add_texture(
        loc("minecraft:block/c"),
        solid(16, 16, [3, 3, 3, 255]),
        None,
    );
    let second = b2.build().unwrap();

    assert_eq!(first.width, second.width);
    assert_eq!(first.height, second.height);
    assert_eq!(first.rgba, second.rgba, "atlas bytes must be identical");
    // UVs identical for every sprite.
    for s in first.sprites() {
        let t = second.sprite(&s.location).unwrap();
        assert_eq!(s.uv_min, t.uv_min);
        assert_eq!(s.uv_max, t.uv_max);
        assert_eq!((s.x, s.y), (t.x, t.y));
    }
}

#[test]
fn handles_non_uniform_sizes() {
    let mut b = AtlasBuilder::new();
    b.add_texture(
        loc("minecraft:block/big"),
        solid(64, 64, [9, 9, 9, 255]),
        None,
    );
    b.add_texture(
        loc("minecraft:block/small"),
        solid(16, 16, [1, 1, 1, 255]),
        None,
    );
    b.add_texture(
        loc("minecraft:block/mid"),
        solid(32, 32, [5, 5, 5, 255]),
        None,
    );
    let atlas = b.build().unwrap();
    // Every sprite retains its native size (no scaling policy).
    assert_eq!(atlas.sprite(&loc("minecraft:block/big")).unwrap().width, 64);
    assert_eq!(atlas.sprite(&loc("minecraft:block/mid")).unwrap().width, 32);
    assert_eq!(
        atlas.sprite(&loc("minecraft:block/small")).unwrap().width,
        16
    );
    // Atlas must be at least as wide as the widest sprite.
    assert!(atlas.width >= 64);
    // Sprites must not overlap.
    assert_no_overlap(&atlas);
}

#[test]
fn animation_strip_frames_are_addressable() {
    // A 16x64 strip = 4 frames of 16x16.
    let img = solid(16, 64, [7, 7, 7, 255]);
    let meta = TextureMeta::parse(br#"{"animation":{"frametime":3}}"#).unwrap();
    let mut b = AtlasBuilder::new().with_width(16);
    b.add_texture(loc("minecraft:block/water_still"), img, Some(meta));
    let atlas = b.build().unwrap();
    let s = atlas.sprite(&loc("minecraft:block/water_still")).unwrap();
    assert!(s.is_animated());
    assert_eq!(s.frame_count, 4);
    assert_eq!(s.frame_height, 16);
    assert_eq!(s.frametime, 3);
    assert_eq!(s.frames.len(), 4);
    // Frame 2 sits two frame-heights down.
    assert_eq!(s.frame_pixel_rect(2), Some([s.x, s.y + 32, 16, 16]));
    assert_eq!(s.frame_pixel_rect(4), None);
    let (uv_min, uv_max) = s.frame_uv(0, atlas.width, atlas.height).unwrap();
    assert_eq!(uv_min, [0.0, 0.0]);
    assert_eq!(uv_max[0], 1.0);
    // One frame is 1/4 of a 64-tall atlas region... atlas height is padded to
    // pow2 (64), so a 16px frame spans 0.25 vertically.
    assert_eq!(uv_max[1], 16.0 / atlas.height as f32);
}

#[test]
fn animation_explicit_frame_order() {
    let img = solid(16, 48, [1, 1, 1, 255]); // 3 frames
    let meta = TextureMeta::parse(br#"{"animation":{"frames":[0,2,1,2]}}"#).unwrap();
    let mut b = AtlasBuilder::new();
    b.add_texture(loc("minecraft:block/anim"), img, Some(meta));
    let atlas = b.build().unwrap();
    let s = atlas.sprite(&loc("minecraft:block/anim")).unwrap();
    assert_eq!(s.frame_count, 3); // physical frames
    assert_eq!(s.frames.len(), 4); // playback order
    let order: Vec<u32> = s.frames.iter().map(|f| f.index).collect();
    assert_eq!(order, vec![0, 2, 1, 2]);
}

#[test]
fn interpolation_and_per_frame_times_reach_the_sprite() {
    // `interpolate` plus explicit per-frame times must survive into the baked
    // sprite unchanged: the renderer drives interpolation purely from this data,
    // sampling frame N and N+1 (both resident in the immutable atlas) and
    // blending in-shader, so the atlas never needs a per-tick re-upload.
    let img = solid(16, 48, [4, 4, 4, 255]); // 3 physical frames
    let meta = TextureMeta::parse(
        br#"{"animation":{"interpolate":true,"frametime":2,"frames":[0,{"index":1,"time":5},2]}}"#,
    )
    .unwrap();
    let mut b = AtlasBuilder::new().with_width(16);
    b.add_texture(loc("minecraft:block/lava_still"), img, Some(meta));
    let atlas = b.build().unwrap();
    let s = atlas.sprite(&loc("minecraft:block/lava_still")).unwrap();

    assert!(
        s.interpolate,
        "interpolate flag must be carried to the sprite"
    );
    assert_eq!(s.frame_count, 3);
    assert_eq!(s.frames.len(), 3);
    // Explicit frames keep their own `time` override; a bare index carries
    // `None`, meaning "fall back to the sprite's frametime" at playback.
    assert_eq!(s.frames[0].index, 0);
    assert_eq!(s.frames[0].time, None);
    assert_eq!(s.frames[1].index, 1);
    assert_eq!(s.frames[1].time, Some(5));
    assert_eq!(s.frames[2].index, 2);
    assert_eq!(s.frames[2].time, None);
    assert_eq!(s.frametime, 2, "default frametime carried for None frames");
    // The next frame after N=1 is addressable in the same region (no separate
    // dynamic texture is needed to interpolate towards it).
    assert!(s.frame_pixel_rect(2).is_some());
}

#[test]
fn bad_animation_strip_errors() {
    // 16x20 with 16px frames does not divide evenly.
    let img = solid(16, 20, [1, 1, 1, 255]);
    let meta = TextureMeta::parse(br#"{"animation":{"height":16}}"#).unwrap();
    let mut b = AtlasBuilder::new();
    b.add_texture(loc("minecraft:block/bad"), img, Some(meta));
    assert!(matches!(
        b.build(),
        Err(AtlasError::BadAnimationStrip { .. })
    ));
}

#[test]
fn last_texture_wins_on_duplicate() {
    let mut b = AtlasBuilder::new().with_width(16);
    b.add_texture(
        loc("minecraft:block/x"),
        solid(16, 16, [1, 1, 1, 255]),
        None,
    );
    b.add_texture(
        loc("minecraft:block/x"),
        solid(16, 16, [2, 2, 2, 255]),
        None,
    );
    let atlas = b.build().unwrap();
    assert_eq!(atlas.sprites().len(), 1);
    assert_eq!(&atlas.rgba[0..4], &[2, 2, 2, 255]);
}

/// Asserts no two sprites' placed regions overlap.
fn assert_no_overlap(atlas: &lodestone_assets::Atlas) {
    let s = atlas.sprites();
    for i in 0..s.len() {
        for j in i + 1..s.len() {
            let a = &s[i];
            let b = &s[j];
            let disjoint = a.x + a.width <= b.x
                || b.x + b.width <= a.x
                || a.y + a.height <= b.y
                || b.y + b.height <= a.y;
            assert!(
                disjoint,
                "sprites {} and {} overlap",
                a.location, b.location
            );
        }
    }
}

#[test]
fn padding_insets_sprite_and_extrudes_gutter() {
    // A 16x16 tile with a 2px gutter: the sprite sits at (pad, pad) and the
    // gutter is filled by extruding its edge pixels (edge clamp).
    let color = [7, 8, 9, 255];
    let mut b = AtlasBuilder::new().with_padding(2);
    b.add_texture(loc("minecraft:block/stone"), solid(16, 16, color), None);
    let atlas = b.build().unwrap();
    let sprite = atlas.sprite(&loc("minecraft:block/stone")).unwrap();

    // Sprite is inset by the padding; UVs point at the interior.
    assert_eq!((sprite.x, sprite.y), (2, 2));
    assert_eq!((sprite.width, sprite.height), (16, 16));
    assert_eq!(
        sprite.uv_min,
        [2.0 / atlas.width as f32, 2.0 / atlas.height as f32]
    );
    assert_eq!(
        sprite.uv_max,
        [18.0 / atlas.width as f32, 18.0 / atlas.height as f32]
    );

    let px = |x: u32, y: u32| {
        let i = ((y * atlas.width + x) * 4) as usize;
        [
            atlas.rgba[i],
            atlas.rgba[i + 1],
            atlas.rgba[i + 2],
            atlas.rgba[i + 3],
        ]
    };
    // Interior pixel is the sprite colour...
    assert_eq!(px(2, 2), color);
    // ...and so is the whole gutter, including the extruded corner at (0,0).
    assert_eq!(px(0, 0), color);
    assert_eq!(px(1, 8), color);
    assert_eq!(px(8, 0), color);
}

#[test]
fn padding_keeps_sprites_disjoint_and_deterministic() {
    let build = || {
        let mut b = AtlasBuilder::new().with_padding(1);
        b.add_texture(
            loc("minecraft:block/a"),
            solid(16, 16, [10, 0, 0, 255]),
            None,
        );
        b.add_texture(
            loc("minecraft:block/b"),
            solid(16, 16, [0, 20, 0, 255]),
            None,
        );
        b.add_texture(loc("minecraft:block/c"), solid(8, 8, [0, 0, 30, 255]), None);
        b.build().unwrap()
    };
    let a1 = build();
    let a2 = build();
    assert_no_overlap(&a1);
    assert_eq!(a1.rgba, a2.rgba);
    assert_eq!(a1.sprites(), a2.sprites());
    // Gutter guarantees at least one pixel between neighbours in atlas space.
    for s in a1.sprites() {
        assert!(
            s.x >= 1 && s.y >= 1,
            "sprite {} touches the atlas edge",
            s.location
        );
    }
}
