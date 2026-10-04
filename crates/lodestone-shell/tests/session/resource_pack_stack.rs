//! Resource packs on disk reach the production
//! [`lodestone_assets::ResourceManager`] stack in the right priority direction.
//!
//! ## What it is
//!
//! One gate over `resources::{scan_resource_packs_in, set_selected_packs,
//! open_pack_stack}` — the three functions the Resource Packs screen actually
//! calls. It drives `open_pack_stack`, the same function every `load_*` in
//! `resources.rs` goes through (the block atlas, the block models, the GUI and
//! item atlases, the sky, the container panels), rather than reassembling an
//! equivalent stack here: an equivalent stack would pass whether or not the
//! screen is wired to anything.
//!
//! ## Why it is `#[ignore]`d
//!
//! `open_pack_stack` needs the staged built-in archive at the bottom of the stack,
//! which is not repo state. Same convention as the other resource gates: a
//! missing pack must fail *loud* rather than pass vacuously, so the assertion
//! that the archive loaded is inside the test and the test is opt-in.
//!
//! ```text
//! LODESTONE_PACKS_FIXTURE=/private/tmp/lt-packs-spot \
//!   cargo test -p lodestone-shell --test session resource_pack_stack -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `LODESTONE_PACKS_FIXTURE` points at a directory holding a `resourcepacks/`
//! folder with two packs that override the same built-in path with different
//! bytes — one a directory tree, one a `.zip`. The fixture is built by hand
//! rather than by this test because `lodestone-shell` has no zip *writer* (the
//! `zip` crate is `lodestone-assets`' dependency, not a dev dependency here),
//! and adding one for a fixture would be a production dependency paid for by a
//! test.

use std::path::{Path, PathBuf};

use lodestone::resources::{
    PackKind, open_pack_stack, scan_resource_packs_in, set_selected_packs,
};

/// The built-in path both fixture packs override.
const OVERRIDDEN: &str = "assets/minecraft/textures/block/stone.png";
/// A path neither fixture pack carries, so it comes from the built-in archive.
/// This proves the base layer is still underneath the selected packs.
const BUILT_IN_ONLY: &str = "assets/minecraft/textures/block/dirt.png";

fn fixture_dir() -> PathBuf {
    PathBuf::from(
        std::env::var_os("LODESTONE_PACKS_FIXTURE")
            .expect("set LODESTONE_PACKS_FIXTURE — see this file's module doc"),
    )
    .join("resourcepacks")
}

/// The current version's cache root when it is a complete pack.
fn pack_root() -> Option<PathBuf> {
    lodestone_mc_cache::cache_root().filter(|root| complete_pack(root))
}

fn complete_pack(dir: &Path) -> bool {
    dir.join("lodestone-resources.zip").is_file()
        && dir.join("generated/reports/blocks.json").is_file()
}

#[test]
#[ignore = "needs LODESTONE_PACKS_FIXTURE and the staged built-in resource archive"]
fn a_folder_and_a_zip_are_both_discovered_and_the_top_of_the_order_wins() {
    let dir = fixture_dir();
    let packs = scan_resource_packs_in(&dir);
    assert_eq!(
        packs.len(),
        2,
        "expected one folder pack and one zip in {}, found {:?}",
        dir.display(),
        packs.iter().map(|p| &p.id).collect::<Vec<_>>()
    );

    // Both kinds, both descriptions, both icons. The zip's description is a
    // *text component* and the folder's a plain string — `PackMeta` accepts
    // either, and this is the only place that is exercised against real files.
    let folder = packs
        .iter()
        .find(|p| p.kind == PackKind::Directory)
        .expect("a directory pack");
    let zip = packs
        .iter()
        .find(|p| p.kind == PackKind::Zip)
        .expect("a zip pack");
    for pack in [folder, zip] {
        assert!(
            !pack.description.is_empty(),
            "{}: pack.mcmeta description did not reach the entry",
            pack.id
        );
        let icon = pack
            .icon
            .as_ref()
            .unwrap_or_else(|| panic!("{}: pack.png did not decode", pack.id));
        assert!(icon.width > 0 && icon.height > 0, "{}: empty icon", pack.id);
        assert_eq!(
            icon.rgba.len(),
            (icon.width * icon.height * 4) as usize,
            "{}: icon is not RGBA8",
            pack.id
        );
        println!(
            "discovered {} ({:?}) {}x{} icon — {:?}",
            pack.id, pack.kind, icon.width, icon.height, pack.description
        );
    }

    let root = pack_root().expect("no staged pack root; set LODESTONE_ASSETS");

    // With nothing selected, the overridden path comes from the built-in archive.
    set_selected_packs(Vec::new());
    let built_in = open_pack_stack(&root)
        .expect("the built-in resource archive must open")
        .read(OVERRIDDEN)
        .expect("the built-in archive has a stone texture");
    let built_in_only = open_pack_stack(&root)
        .expect("the built-in resource archive must open")
        .read(BUILT_IN_ONLY)
        .expect("the built-in archive has a dirt texture");

    // `scan_resource_packs_in` took an explicit path, but the stack goes through
    // `scan_resource_packs()`, which reads the real data dir — so the fixture has
    // to be reachable that way too. `LODESTONE_DATA_DIR` is what points there.
    assert_eq!(
        std::fs::canonicalize(crate_packs_dir()).ok(),
        std::fs::canonicalize(&dir).ok(),
        "set LODESTONE_DATA_DIR to LODESTONE_PACKS_FIXTURE so the production \
         scan sees the same folder this test just listed"
    );

    // The **first** id is the highest priority — the UI's top row. The expected
    // value is the fixture's own flat colour, not bytes read back through the
    // reader under test: the folder pack's stone is magenta and the zip's is
    // cyan (`docs/resource-packs-screen.md`'s recipe), so each arm asserts a
    // value that originates outside this crate entirely, and the *other* arm's
    // colour is the control that a stale or reversed stack would produce.
    for (top, other) in [(zip, folder), (folder, zip)] {
        let expect_rgb = if top.kind == PackKind::Zip {
            [0u8, 255, 255] // cyan
        } else {
            [255u8, 0, 255] // magenta
        };
        let wrong_rgb = if top.kind == PackKind::Zip {
            [255u8, 0, 255]
        } else {
            [0u8, 255, 255]
        };

        set_selected_packs(vec![top.id.clone(), other.id.clone()]);
        let manager = open_pack_stack(&root).expect("the built-in resource archive must open");
        assert_eq!(manager.len(), 3, "built-in archive + two packs");

        let winner = manager.read(OVERRIDDEN).expect("something must serve it");
        assert_ne!(
            winner, built_in,
            "{}: the override resolved to the built-in texture, so the pack \
             reached nothing",
            top.id
        );
        let img = lodestone_assets::Image::decode_png(&winner)
            .expect("the winning texture must be a decodable PNG");
        let px = [img.rgba[0], img.rgba[1], img.rgba[2]];
        assert_eq!(
            px, expect_rgb,
            "with {} at the top of the order, its own texture must win. Reading \
             {wrong_rgb:?} here means the stack is reversed — the UI's top row is \
             the LAST element of `ResourceManager`'s stack, not the first",
            top.id
        );

        // The built-in pack is still underneath.
        assert_eq!(
            manager.read(BUILT_IN_ONLY).as_ref(),
            Some(&built_in_only),
            "a path neither selected pack carries must still come from the built-in archive"
        );
        println!("{} on top -> {OVERRIDDEN} is rgb{px:?}", top.id);
    }
}

/// The last link: the pack's pixels are in the **stitched block atlas** the
/// world renderer binds, not merely readable through the manager.
///
/// This is what "a texture actually changes in-game" reduces to without a GPU:
/// `BlockResources::load(true)` is the call `sim/build.rs` makes on a live
/// session, `BlockAtlas::build` stitches from the manager it opens, and
/// `Atlas::rgba` is the buffer uploaded verbatim. So reading the fixture's
/// magenta out of `minecraft:block/stone`'s placed region is the same pixel the
/// mesher's UVs will sample.
///
/// Separate from the gate above so the two failure modes are distinguishable: a
/// reversed stack fails there, a stack that never reaches the atlas fails here.
#[test]
#[ignore = "needs LODESTONE_PACKS_FIXTURE and the staged resource archive + blocks.json"]
fn the_selected_packs_pixels_reach_the_stitched_block_atlas() {
    use lodestone::resources::BlockResources;
    use lodestone_assets::ResourceLocation;

    let packs = scan_resource_packs_in(&fixture_dir());
    let folder = packs
        .iter()
        .find(|p| p.kind == PackKind::Directory)
        .expect("a directory pack");

    // Control first: with no pack selected, stone is *not* the fixture colour.
    // Without this the assertion below cannot tell a working override from a
    // vanilla stone texture that happens to be magenta-ish.
    set_selected_packs(Vec::new());
    let plain = stone_pixel(&BlockResources::load(true));
    assert_ne!(
        plain,
        [255, 0, 255],
        "control: vanilla stone must not already be the fixture's magenta, or \
         the assertion below proves nothing"
    );
    println!("no packs -> block/stone is rgb{plain:?}");

    set_selected_packs(vec![folder.id.clone()]);
    let with_pack = stone_pixel(&BlockResources::load(true));
    assert_eq!(
        with_pack,
        [255, 0, 255],
        "the folder pack's magenta stone must be in the stitched atlas; got \
         rgb{with_pack:?} (the vanilla texture is rgb{plain:?}), so the pack \
         reached the manager but not the atlas"
    );
    println!("folderpack selected -> block/stone is rgb{with_pack:?}");

    /// The top-left pixel of `minecraft:block/stone`'s placed region in the
    /// stitched atlas.
    fn stone_pixel(resources: &BlockResources) -> [u8; 3] {
        let atlas = resources
            .vanilla_atlas
            .as_ref()
            .unwrap_or_else(|| {
                panic!(
                    "vanilla assets did not load; banner: {:?}",
                    resources.banner
                )
            })
            .atlas();
        let stone = ResourceLocation::parse("minecraft:block/stone").expect("a valid location");
        let sprite = atlas
            .sprite(&stone)
            .expect("block/stone must be in the block atlas");
        let i = ((sprite.y * atlas.width + sprite.x) * 4) as usize;
        [atlas.rgba[i], atlas.rgba[i + 1], atlas.rgba[i + 2]]
    }
}

/// The production scan's directory, via the same helper `resources` uses.
fn crate_packs_dir() -> PathBuf {
    lodestone::resources::resource_packs_dir()
}
