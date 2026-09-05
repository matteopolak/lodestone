use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const PROTO: &str = "proto/lodestone/storage/v1/storage.proto";
const GENERATED: &str = "src/generated/lodestone.storage.v1.rs";
const DESCRIPTOR: &str = "proto/lodestone/storage/v1/storage.fds.bin";
const REGENERATE_ENV: &str = "LODESTONE_STORAGE_SCHEMA_REGENERATE";

fn main() {
    println!("cargo::rerun-if-changed={PROTO}");
    println!("cargo::rerun-if-changed={GENERATED}");
    println!("cargo::rerun-if-changed={DESCRIPTOR}");
    println!("cargo::rerun-if-env-changed={REGENERATE_ENV}");

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env::var("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    let generated_out = out.join("schema-generated");
    fs::create_dir_all(&generated_out).expect("create schema generation directory");

    let mut config = prost_build::Config::new();
    config.out_dir(&generated_out);
    config.file_descriptor_set_path(out.join("storage.fds.bin"));
    config.protoc_executable(
        protoc_bin_vendored::protoc_bin_path().expect("vendored protoc is available"),
    );
    config
        .compile_protos(&[manifest.join(PROTO)], &[manifest.join("proto")])
        .expect("compile storage protobuf schema");

    let generated = generated_out.join("lodestone.storage.v1.rs");
    let descriptor = out.join("storage.fds.bin");
    let regenerate = env::var_os(REGENERATE_ENV).is_some_and(|value| value == "1");
    sync_or_check(&generated, &manifest.join(GENERATED), regenerate);
    sync_or_check(&descriptor, &manifest.join(DESCRIPTOR), regenerate);
}

fn sync_or_check(actual: &Path, committed: &Path, regenerate: bool) {
    let actual = fs::read(actual).unwrap_or_else(|error| {
        panic!("read generated schema artifact {}: {error}", actual.display())
    });
    if regenerate {
        fs::write(committed, actual).unwrap_or_else(|error| {
            panic!("write regenerated schema artifact {}: {error}", committed.display())
        });
        return;
    }

    match fs::read(committed) {
        Ok(expected) if expected == actual => {}
        Ok(_) => panic!(
            "storage schema generated artifact drifted: {}. Run \
             `LODESTONE_STORAGE_SCHEMA_REGENERATE=1 cargo check -p lodestone-storage-schema` \
             and commit the result.",
            committed.display(),
        ),
        Err(error) => panic!(
            "storage schema generated artifact {} is missing ({error}). Run \
             `LODESTONE_STORAGE_SCHEMA_REGENERATE=1 cargo check -p lodestone-storage-schema`.",
            committed.display(),
        ),
    }
}
