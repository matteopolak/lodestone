//! Regression gate: **break debris must take its block's tint**, or foliage and
//! redstone throw white flecks.
//!
//! # The bug this reproduces
//!
//! A user reported: *"if I break a block that causes another to break, the
//! particles for the other block are white."* The wire was innocent — a live
//! capture (`lodestone-v770`'s `live_destroy_block_event` gate) shows
//! `level_event` 2001 carrying the cascaded block's correct block-state id, and
//! that id resolves to the correct `#particle` sprite. What was missing is the
//! *other* half of vanilla's break-particle construction: the base colour
//! starts at a fixed grey (0.6 in each channel), then — if the block state has
//! a biome tint source at tint index 0 — that source's colour (evaluated for
//! this block, level and position) is multiplied into each channel.
//!
//! Both shell emit sites passed a hardcoded `[1.0; 3]` there. The tinted blocks
//! are *exactly* the ones whose atlas sprites are **greyscale** (`grass`,
//! `fern`, the leaves, `sugar_cane`, `redstone_dust_*`), because vanilla stores
//! them grey and colours them at draw time — so the missing multiply did not
//! desaturate the debris a little, it rendered it near-white. Measured:
//! `redstone_wire` debris came out `#cbcbcb`, `short_grass` `#676667`.
//!
//! # Why the report reads as "the *cascading* block"
//!
//! Not because the two emit paths mask each other. Because of **which blocks
//! cascade**: the block a player punches is nearly always an untinted one
//! (stone, dirt, planks, ore) for which `[1.0; 3]` is the right answer, while
//! the block that pops when its support goes is nearly always foliage or wiring
//! — grass, fern, sugar cane, vine, lily pad, redstone wire — i.e. the tinted
//! set. The asymmetry is in the block population, not in the code path, which is
//! why "break something and look at the debris" never reproduced it.
//!
//! # What this gate does that the existing one cannot
//!
//! `break_particles_pixels.rs` renders the **demo palette** (`Mode::Headless`,
//! `Sim::new`), which has no colormaps and no tinted blocks — so it structurally
//! cannot exercise a tint at all. That is the *world* species of vacuous test
//! (`CLAUDE.md`): exemplary source, pointed at the one scene in the tree that
//! cannot contain the structure under test. This gate uses the **real vanilla
//! atlas** and drives `Particles::destroy_block` **in isolation** — no
//! `breaking_block` flecks, no local prediction — which is exactly the shape of
//! the `NetUpdate::BlockDestroyed` path a cascading break takes.
//!
//! Every assertion is paired with a **control that is executed and observed to
//! fail**: the same fragments, recomputed with the pre-fix `[1.0; 3]`, must be
//! grey where the subject is coloured. A control that merely *would* fail is not
//! evidence.
//!
//! Run it explicitly (it needs `.cache/mc/<version>/client.jar` +
//! `generated/reports/blocks.json`, and per §12.52 it **fails** rather than
//! skips when they are missing):
//!
//! ```text
//! cargo test -p lodestone-shell --test break_particle_tint -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use lodestone::particles::Particles;
use lodestone_assets::{ResourceManager, ResourceSource, ZipSource};
use lodestone_model::BlockStateRegistry;
use lodestone_render::{BlockModels, Camera, blocks_json_registry};

/// Walk up from the test's working directory for a pack root holding both files
/// the atlas needs, mirroring `crate::resources::asset_root` (which is private).
fn pack_root() -> PathBuf {
    let cwd = std::env::current_dir().expect("cwd");
    for base in cwd.ancestors() {
        let cache = base.join(".cache/mc");
        let Ok(entries) = std::fs::read_dir(&cache) else {
            continue;
        };
        let mut roots: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.join("client.jar").is_file() && p.join("generated/reports/blocks.json").is_file()
            })
            .collect();
        roots.sort();
        if let Some(best) = roots.pop() {
            return best;
        }
    }
    panic!(
        "no vanilla pack found under any ancestor's .cache/mc/<version>/ (needs client.jar + \
         generated/reports/blocks.json). This gate fails rather than skips: a skip reads as a pass."
    );
}

fn load_models(root: &std::path::Path) -> BlockModels {
    let bytes = std::fs::read(root.join("client.jar")).expect("read client.jar");
    let zip = ZipSource::from_bytes(bytes).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>]);
    let registry =
        blocks_json_registry(&root.join("generated/reports/blocks.json")).expect("blocks.json");
    BlockModels::build(&manager, &registry).expect("bake block models")
}

fn registry_of(root: &std::path::Path) -> impl BlockStateRegistry {
    blocks_json_registry(&root.join("generated/reports/blocks.json")).expect("blocks.json")
}

/// `block name -> (first state id, that block's every state id)`, built in one
/// pass. A per-lookup linear scan is 32,366 `resolve` calls, each of which hands
/// back an owned property map — twenty of those dominated this gate's runtime.
fn state_index(reg: &impl BlockStateRegistry) -> BTreeMap<String, u32> {
    let mut out: BTreeMap<String, u32> = BTreeMap::new();
    for id in 0..reg.state_count() {
        if let Some(state) = reg.resolve(id) {
            out.entry(state.block.to_string()).or_insert(id);
        }
    }
    out
}

/// sRGB byte -> linear, matching a `Rgba8UnormSrgb` texture fetch.
fn to_linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear -> sRGB byte, matching the write into an sRGB colour target.
fn to_srgb(c: f32) -> u8 {
    let c = if c <= 0.0031_308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    #[expect(clippy::cast_possible_truncation, reason = "clamped to 0..=255")]
    {
        (c * 255.0).round().clamp(0.0, 255.0) as u8
    }
}

/// One destroy burst, measured two ways: the mean **on-screen** colour of its
/// visible fragments as the shipped code tints them (`subject`) and as the
/// pre-fix hardcoded `[1.0; 3]` tinted them (`control`).
///
/// Both come from the **same** burst — one `Particles`, one RNG stream, one set
/// of quads — because the particle engine is not seeded deterministically across
/// instances, so two separate bursts of the same block are two different
/// samples and could not be compared fragment-for-fragment. Deriving the control
/// by substituting the tint back out of the extracted instance colour keeps the
/// only difference between the two numbers the multiplier under test.
///
/// The arithmetic is a CPU model of `particles.rs`'s fragment shader, deliberately
/// step-for-step: nearest tap at the quad centre (the shader's `mag_filter:
/// Nearest`, which is what a magnified billboard gets), `texel * colour`,
/// alpha-discard below `0.02`, sRGB in and out.
struct Burst {
    subject: [u8; 3],
    control: [u8; 3],
    visible: usize,
}

/// `p` is threaded in rather than built per call because `Particles::new`
/// materialises two 32,366-entry tables, which dominates the census below by an
/// order of magnitude over the burst itself.
fn burst(p: &mut Particles, models: &BlockModels, state: u32) -> Burst {
    let atlas = models.atlas();
    p.engine_mut().clear();
    p.destroy_block([0, 64, 0], state, [1.0; 3]);
    let _ = p.extract(&Camera::default(), 0.0, &|_, _, _| {
        Some(lodestone_particle::FULL_BRIGHT)
    });

    // The state's own tint, already folded into the instance colour as
    // `base * tint` where `base` is vanilla's break-particle 0.6 grey times the
    // light shade — channel-independent, so the control's untinted colour is recovered
    // by *dividing the tint back out of one channel*, not by dividing every
    // channel. That distinction is load-bearing: `redstone_wire` at power 0 has
    // a tint of `[0.3, 0.0, 0.0]`, so two of its channels are exactly zero and a
    // per-channel division reconstructs a red control instead of the grey one
    // the pre-fix code actually produced.
    let own = models.particle_tint(state).unwrap_or([1.0; 3]);
    let widest = (0..3).max_by(|a, b| own[*a].total_cmp(&own[*b])).unwrap();
    assert!(
        own[widest] > 0.0,
        "state {state} has an all-zero particle tint; the control cannot be reconstructed from it"
    );

    let mut subj = [0f32; 3];
    let mut ctrl = [0f32; 3];
    let mut visible = 0usize;
    for inst in p.instances() {
        let raw = bytemuck::bytes_of(inst);
        let f = |i: usize| f32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap());
        // `ParticleInstance`: centre_size[0..4], uv[4..8], colour[8..12], roll[12..16].
        let (uc, vc) = ((f(4) + f(6)) * 0.5, (f(5) + f(7)) * 0.5);
        #[expect(clippy::cast_possible_truncation, reason = "UVs are in 0..1")]
        let (px, py) = (
            ((uc * atlas.width as f32) as u32).min(atlas.width - 1),
            ((vc * atlas.height as f32) as u32).min(atlas.height - 1),
        );
        let i = ((py * atlas.width + px) * 4) as usize;
        let texel = &atlas.rgba[i..i + 4];
        if f32::from(texel[3]) / 255.0 < 0.02 {
            continue;
        }
        visible += 1;
        // `base` == the untinted `[1.0; 3]` colour the pre-fix emit sites produced.
        let base = f(8 + widest) / own[widest];
        for c in 0..3 {
            let lit = to_linear(texel[c]);
            subj[c] += lit * f(8 + c);
            ctrl[c] += lit * base;
        }
    }
    if visible == 0 {
        return Burst {
            subject: [0; 3],
            control: [0; 3],
            visible: 0,
        };
    }
    #[expect(clippy::cast_precision_loss, reason = "counts are at most 64")]
    let n = visible as f32;
    Burst {
        subject: [
            to_srgb(subj[0] / n),
            to_srgb(subj[1] / n),
            to_srgb(subj[2] / n),
        ],
        control: [
            to_srgb(ctrl[0] / n),
            to_srgb(ctrl[1] / n),
            to_srgb(ctrl[2] / n),
        ],
        visible,
    }
}

/// Grey means every channel within `8/255` of every other — the signature of
/// debris that took no tint at all.
fn is_grey(rgb: [u8; 3]) -> bool {
    let max = rgb.iter().copied().max().unwrap();
    let min = rgb.iter().copied().min().unwrap();
    u32::from(max) - u32::from(min) <= 8
}

/// A cascading break's debris must take its block's tint. Subject and control
/// differ **only** in that multiplier.
#[test]
#[ignore = "requires a fetched vanilla client.jar + blocks.json under .cache/mc/<version>/"]
fn cascading_block_debris_is_tinted_not_grey() {
    let root = pack_root();
    let models = load_models(&root);
    let registry = registry_of(&root);

    // Anti-vacuity: a table that resolved no tints at all satisfies every
    // "is not grey" assertion below by never reaching one.
    let tinted = models.particle_tinted_state_count();
    eprintln!("=== break-particle tint gate (pack {})", root.display());
    eprintln!("states carrying a particle tint = {tinted} / {}", models.state_count());
    assert!(
        tinted > 500,
        "only {tinted} states resolved a particle tint; the colormaps did not load, so every \
         assertion below would pass for the wrong reason"
    );
    assert!(
        Particles::new(Some(&models)).tinted_state_count() > 500,
        "BlockModels resolved tints but Particles did not copy them into its own table — the \
         island case: the data exists and nothing consumes it"
    );

    // Blocks that cascade when their support is removed **and** whose vanilla
    // `#particle` sprite is stored greyscale — the population the user actually
    // saw as white. `sugar_cane` is deliberately *not* here: its sprite is
    // already green on disk, so losing the tint desaturates it rather than
    // whitening it, and asserting "the control is grey" for it would be a
    // control that cannot fire. It is covered by the every-tinted-subject check
    // below instead.
    let subjects = [
        "minecraft:short_grass",
        "minecraft:fern",
        "minecraft:tall_grass",
        "minecraft:oak_leaves",
        "minecraft:vine",
        "minecraft:redstone_wire",
    ];
    let index = state_index(&registry);
    let mut particles = Particles::new(Some(&models));
    let mut grey_controls = 0usize;
    for name in subjects {
        let state = *index
            .get(name)
            .unwrap_or_else(|| panic!("{name} is not in blocks.json"));
        let tint = models
            .particle_tint(state)
            .unwrap_or_else(|| panic!("{name} must carry a particle tint (vanilla registers one)"));
        let Burst {
            subject,
            control,
            visible,
        } = burst(&mut particles, &models, state);
        eprintln!(
            "{name:26} state={state:5} tint={tint:?}  subject=#{:02x}{:02x}{:02x} \
             control(no tint)=#{:02x}{:02x}{:02x}  visible={visible}",
            subject[0], subject[1], subject[2], control[0], control[1], control[2]
        );

        assert!(
            visible > 0,
            "{name} threw no visible debris, so the colour assertion would be vacuous"
        );
        // The control is the pre-fix code. Observing it grey here — every run,
        // not hypothetically — is what proves the detector fires.
        assert!(
            is_grey(control),
            "{name}: the pre-fix control came out #{:02x}{:02x}{:02x}, which is not grey. The \
             detector this gate relies on does not fire, so the subject assertion below proves \
             nothing about the tint.",
            control[0],
            control[1],
            control[2]
        );
        grey_controls += 1;
        assert!(
            !is_grey(subject),
            "{name}: debris is grey (#{:02x}{:02x}{:02x}) — the block's particle tint {tint:?} \
             never reached the emitter. This is the reported bug: a greyscale sprite with no \
             tint renders as white flecks.",
            subject[0],
            subject[1],
            subject[2]
        );
    }
    assert_eq!(
        grey_controls,
        subjects.len(),
        "every subject must have had its control observed grey"
    );

    // Every tinted block, greyscale sprite or not, must come out *different*
    // from the untinted control — otherwise the multiplier is reaching the
    // emitter for some blocks and silently not for others. `sugar_cane` and
    // `lily_pad` are the cases this catches that the grey check above cannot.
    for name in [
        "minecraft:sugar_cane",
        "minecraft:lily_pad",
        "minecraft:spruce_leaves",
        "minecraft:melon_stem",
    ] {
        let state = *index.get(name).unwrap_or_else(|| panic!("{name} missing"));
        let b = burst(&mut particles, &models, state);
        eprintln!(
            "{name:26} state={state:5} subject=#{:02x}{:02x}{:02x} control=#{:02x}{:02x}{:02x}",
            b.subject[0], b.subject[1], b.subject[2], b.control[0], b.control[1], b.control[2]
        );
        assert!(b.visible > 0, "{name} threw no visible debris");
        assert_ne!(
            b.subject, b.control,
            "{name} carries a particle tint but its debris is identical to the untinted control, \
             so the tint is not reaching the emitter for it"
        );
    }

    // Untinted blocks must stay untinted — the fix must not tint the world.
    // `grass_block` is the one that catches a fix built on the *face* tint
    // rather than the particle tint: vanilla special-cases its break-particle
    // tint source to "no tint" precisely because its `#particle` is
    // `block/dirt`, so a face-derived fix throws green dirt.
    for name in [
        "minecraft:stone",
        "minecraft:dirt",
        "minecraft:grass_block",
        "minecraft:torch",
        "minecraft:oak_planks",
        "minecraft:cherry_leaves",
    ] {
        let state = *index.get(name).unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(
            models.particle_tint(state),
            None,
            "{name} must have no particle tint; vanilla registers no layer-0 tint source for it \
             (or, for grass_block, special-cases the break-particle tint to none so its block/dirt \
             particle sprite is not tinted green)"
        );
        let Burst {
            subject, control, ..
        } = burst(&mut particles, &models, state);
        assert_eq!(
            subject, control,
            "{name} is untinted, so the fix must be a no-op for it; got \
             #{:02x}{:02x}{:02x} vs #{:02x}{:02x}{:02x}",
            subject[0], subject[1], subject[2], control[0], control[1], control[2]
        );
    }
}

/// Census over **every** tinted state, so the claim is a count rather than a
/// handful of hand-picked examples: how many states' debris was grey before the
/// tint reached the emitter, and how many still are after.
#[test]
#[ignore = "requires a fetched vanilla client.jar + blocks.json under .cache/mc/<version>/"]
fn no_tinted_state_still_throws_grey_debris() {
    let root = pack_root();
    let models = load_models(&root);
    let registry = registry_of(&root);

    // One representative state per distinct (block, tint) pair: 32k bursts would
    // be slow and every state of one block shares its sprite.
    let mut seen: BTreeMap<(String, [u32; 3]), u32> = BTreeMap::new();
    for id in 0..models.state_count() as u32 {
        let Some(tint) = models.particle_tint(id) else {
            continue;
        };
        let Some(state) = registry.resolve(id) else {
            continue;
        };
        #[expect(clippy::cast_possible_truncation, reason = "tints are 0..=1")]
        let key = (
            state.block.to_string(),
            [
                (tint[0] * 255.0) as u32,
                (tint[1] * 255.0) as u32,
                (tint[2] * 255.0) as u32,
            ],
        );
        seen.entry(key).or_insert(id);
    }
    eprintln!(
        "=== particle-tint census: {} distinct (block, tint) pairs over {} tinted states",
        seen.len(),
        models.particle_tinted_state_count()
    );
    assert!(
        seen.len() > 20,
        "only {} tinted (block, tint) pairs — the census is too small to mean anything",
        seen.len()
    );

    let mut particles = Particles::new(Some(&models));
    let mut grey_before = Vec::new();
    let mut grey_after = Vec::new();
    for ((block, _), &id) in &seen {
        let Burst {
            subject,
            control,
            visible,
        } = burst(&mut particles, &models, id);
        if visible == 0 {
            // No visible debris at all (a fully transparent `#particle` window)
            // is a different gap and is not what this census measures.
            continue;
        }
        if is_grey(control) {
            grey_before.push(block.clone());
        }
        if is_grey(subject) {
            grey_after.push(format!("{block} -> #{:02x}{:02x}{:02x}", subject[0], subject[1], subject[2]));
        }
    }
    eprintln!("grey debris BEFORE the tint reached the emitter: {} blocks", grey_before.len());
    eprintln!("grey debris AFTER:                               {} blocks", grey_after.len());

    // The control, as a population: the bug was not one block.
    assert!(
        grey_before.len() > 15,
        "only {} of {} tinted blocks threw grey debris without their tint. The control is too \
         weak to establish that the fix is what changed anything.",
        grey_before.len(),
        seen.len()
    );
    assert!(
        grey_after.is_empty(),
        "{} tinted blocks still throw grey debris: {:?}",
        grey_after.len(),
        grey_after
    );
}
