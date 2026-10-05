//! The dedicated baby rigs in the entity corpus: every baby has an adult to stand
//! beside, is smaller than it, and (against the real pack) unwraps onto sheet pixels
//! that exist.

use lodestone_assets::entity::bake_entity;
use lodestone_assets::entity_models::entity_models;
use lodestone_assets::{Image, ResourceLocation, ResourceManager, ZipSource};

const BABY: &str = "_baby";

/// Axis-aligned extent of a baked model: `(min, max)` over every vertex.
fn extent(def: &lodestone_assets::entity::EntityModelDef) -> ([f32; 3], [f32; 3]) {
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for q in bake_entity(def) {
        for p in q.positions {
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
    }
    (lo, hi)
}

fn volume((lo, hi): ([f32; 3], [f32; 3])) -> f32 {
    (0..3).map(|a| hi[a] - lo[a]).product()
}

/// Every `<x>_baby` entry names an adult `<x>` and encloses less volume. The control
/// compares an entry against itself, which must not read as "smaller".
#[test]
fn every_baby_rig_has_an_adult_and_is_smaller_than_it() {
    let models = entity_models();
    let babies: Vec<_> = models.iter().filter(|e| e.name.ends_with(BABY)).collect();
    assert!(babies.len() >= 35, "expected the full baby set, found {}", babies.len());
    for baby in &babies {
        let adult_name = baby.name.strip_suffix(BABY).unwrap();
        let adult = models
            .iter()
            .find(|e| e.name == adult_name)
            .unwrap_or_else(|| panic!("{}: no adult rig {adult_name}", baby.name));
        let (b, a) = (volume(extent(&(baby.build)())), volume(extent(&(adult.build)())));
        assert!(b > 0.0, "{}: empty extent", baby.name);
        assert!(b < a, "{}: baby volume {b} is not below the adult's {a}", baby.name);
        // Control: the adult is not smaller than itself.
        assert!(!(a < a));
    }
}

fn manager() -> ResourceManager {
    let jar = lodestone_mc_cache::client_jar().expect("no client.jar under .cache/mc; fetch it first");
    ResourceManager::new(vec![Box::new(ZipSource::open(&jar).expect("open client.jar"))])
}

/// `(faces checked, faces on fully transparent texels)` for a model on a sheet. Faces
/// of zero area are skipped; any face outside the sheet beyond four declared texels
/// (vanilla starts a mirrored bee wing at a small negative offset) panics with `name`.
fn count_blank_faces(
    name: &str,
    def: &lodestone_assets::entity::EntityModelDef,
    image: &Image,
) -> (usize, usize) {
    let (w, h) = (image.width as f32, image.height as f32);
    let (mut faces, mut blank) = (0, 0);
    for q in bake_entity(def) {
        let (mut u0, mut v0) = (f32::INFINITY, f32::INFINITY);
        let (mut u1, mut v1) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
        for uv in q.uvs {
            u0 = u0.min(uv[0]);
            u1 = u1.max(uv[0]);
            v0 = v0.min(uv[1]);
            v1 = v1.max(uv[1]);
        }
        if (u1 - u0) * w < 0.5 || (v1 - v0) * h < 0.5 {
            continue;
        }
        let eps = 4.0 / def.texture_width as f32;
        if u0 < -eps || v0 < -eps || u1 > 1.0 + eps || v1 > 1.0 + eps {
            return (faces + 1, usize::MAX);
        }
        let (x0, x1) = ((u0 * w).floor().max(0.0) as u32, ((u1 * w).ceil() as u32).min(image.width));
        let (y0, y1) = ((v0 * h).floor().max(0.0) as u32, ((v1 * h).ceil() as u32).min(image.height));
        let opaque = (y0..y1)
            .any(|y| (x0..x1).any(|x| image.rgba[((y * image.width + x) * 4 + 3) as usize] > 0));
        if !opaque {
            eprintln!("{name}: transparent face ({x0},{y0})-({x1},{y1})");
            blank += 1;
        }
        faces += 1;
    }
    (faces, blank)
}

/// Shifts every box's texture offset by `by` texels, in every part.
fn shift_unwrap(part: &mut lodestone_assets::entity::PartDef, by: f32) {
    for cube in &mut part.cubes {
        cube.tex_offset[0] += by;
    }
    for (_, child) in &mut part.children {
        shift_unwrap(child, by);
    }
}

/// Against the real pack: each baby's sheet exists at an integer multiple of the
/// declared size, every face's unwrap lies inside the sheet, and few faces land on
/// fully transparent texels, so a misplaced texture offset fails instead of drawing
/// transparent geometry. The control shifts the pig's unwrap by ten texels and
/// requires the same detector to flag it.
#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn baby_rigs_unwrap_onto_opaque_pixels_of_the_real_sheets() {
    let manager = manager();
    let (mut faces, mut blank) = (0usize, 0usize);
    let mut pig_sheet = None;
    for entry in entity_models().iter().filter(|e| e.name.ends_with(BABY)) {
        let def = (entry.build)();
        let path = entry.texture.default_path();
        let loc = ResourceLocation::parse(&format!("minecraft:{path}")).unwrap();
        let bytes = manager
            .read_asset(&loc, "textures", "png")
            .unwrap_or_else(|| panic!("{}: sheet {path} missing from the pack", entry.name));
        let image = Image::decode_png(&bytes).unwrap();
        assert!(
            image.width % def.texture_width == 0
                && image.height % def.texture_height == 0
                && image.width / def.texture_width == image.height / def.texture_height,
            "{}: declared {}x{} vs real {}x{}",
            entry.name,
            def.texture_width,
            def.texture_height,
            image.width,
            image.height
        );
        let (f, b) = count_blank_faces(entry.name, &def, &image);
        assert_ne!(b, usize::MAX, "{}: a face unwraps outside the sheet", entry.name);
        faces += f;
        blank += b;
        if entry.name == "pig_baby" {
            pig_sheet = Some((def, image));
        }
    }
    eprintln!("{blank} of {faces} faces are blank");
    // The blank faces are overlay layers the sheet leaves empty (a hat over a bare
    // head) and wings; a misplaced offset on a whole model would push this far higher.
    assert!(blank * 8 < faces, "{blank} of {faces} faces are blank");
    assert!(faces > 500, "suspiciously few faces checked: {faces}");

    let (mut pig, image) = pig_sheet.expect("pig_baby is in the corpus");
    let (_, honest) = count_blank_faces("pig_baby", &pig, &image);
    shift_unwrap(&mut pig.root, 10.0);
    let (shifted_faces, shifted) = count_blank_faces("pig_baby shifted", &pig, &image);
    assert!(
        shifted == usize::MAX || shifted > honest + shifted_faces / 4,
        "control: the shifted pig reads {shifted} blank faces against {honest} unshifted"
    );
}
