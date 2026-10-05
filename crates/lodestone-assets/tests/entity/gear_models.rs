//! The worn-gear rigs in the entity corpus against the real equipment sheets: each
//! sheet exists at the size the rig declares, every face unwraps inside it, and the
//! gear's own boxes land on painted texels.

use lodestone_assets::entity::{EntityModelDef, PartDef, bake_entity};
use lodestone_assets::entity_models::gear_entries;
use lodestone_assets::{Image, ResourceLocation, ResourceManager, ZipSource};

fn manager() -> ResourceManager {
    let jar = lodestone_mc_cache::client_jar().expect("no client.jar under .cache/mc; fetch it first");
    ResourceManager::new(vec![Box::new(ZipSource::open(&jar).expect("open client.jar"))])
}

fn load(manager: &ResourceManager, reference: &str) -> Image {
    let loc = ResourceLocation::parse(&format!("minecraft:{reference}")).unwrap();
    let bytes = manager
        .read_asset(&loc, "textures", "png")
        .unwrap_or_else(|| panic!("sheet {reference} missing from the pack"));
    Image::decode_png(&bytes).unwrap()
}

/// `(faces checked, faces on painted texels, faces outside the sheet)`.
fn painted_faces(def: &EntityModelDef, image: &Image) -> (usize, usize, usize) {
    let (w, h) = (image.width as f32, image.height as f32);
    let (mut faces, mut painted, mut outside) = (0, 0, 0);
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
        faces += 1;
        let eps = 4.0 / def.texture_width as f32;
        if u0 < -eps || v0 < -eps || u1 > 1.0 + eps || v1 > 1.0 + eps {
            outside += 1;
            continue;
        }
        let (x0, x1) = ((u0 * w).floor().max(0.0) as u32, ((u1 * w).ceil() as u32).min(image.width));
        let (y0, y1) = ((v0 * h).floor().max(0.0) as u32, ((v1 * h).ceil() as u32).min(image.height));
        if (y0..y1).any(|y| (x0..x1).any(|x| image.rgba[((y * image.width + x) * 4 + 3) as usize] > 0)) {
            painted += 1;
        }
    }
    (faces, painted, outside)
}

fn shift_unwrap(part: &mut PartDef, by: f32) {
    for cube in &mut part.cubes {
        cube.tex_offset[0] += by;
    }
    for (_, child) in &mut part.children {
        shift_unwrap(child, by);
    }
}

#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn gear_rigs_unwrap_onto_painted_pixels_of_the_real_equipment_sheets() {
    let manager = manager();
    let mut pig = None;
    for entry in gear_entries() {
        let def = (entry.build)();
        let image = load(&manager, entry.texture.default_path());
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
        let (faces, painted, outside) = painted_faces(&def, &image);
        eprintln!("{}: {painted} of {faces} faces painted", entry.name);
        assert_eq!(outside, 0, "{}: faces unwrap outside the sheet", entry.name);
        // Gear paints only its own boxes on a sheet that is otherwise empty, so the
        // bar is "the gear is there", not "most of the mesh is".
        assert!(painted >= 3, "{}: only {painted} painted faces", entry.name);
        if entry.name == "pig_saddle" {
            pig = Some((def, image, painted));
        }
    }
    // The three wolf armour crack sheets unwrap on the armour rig.
    let wolf = gear_entries().into_iter().find(|e| e.name == "wolf_armor").unwrap();
    for level in ["low", "medium", "high"] {
        let image = load(&manager, &format!("entity/wolf/wolf_armor_crackiness_{level}"));
        let def = (wolf.build)();
        let (_, painted, outside) = painted_faces(&def, &image);
        assert_eq!(outside, 0, "crack {level}: faces outside the sheet");
        assert!(painted >= 3, "crack {level}: only {painted} painted faces");
    }
    // Control: moving the unwrap ten texels must lose painted faces, or the
    // detector cannot tell a right offset from a wrong one.
    let (mut def, image, honest) = pig.expect("pig_saddle is in the corpus");
    shift_unwrap(&mut def.root, 10.0);
    let (_, shifted, outside) = painted_faces(&def, &image);
    assert!(outside > 0 || shifted < honest, "control: shifted {shifted} vs honest {honest}");
}
