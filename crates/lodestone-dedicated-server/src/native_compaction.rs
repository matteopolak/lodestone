//! Explicit operator entry point for native-store maintenance compaction.
//!
//! A live server owns its native storage handle and may save concurrently, so
//! this command intentionally runs as a separate process against a stopped
//! world. It refuses a missing segment before opening the store, preventing a
//! typo from creating an empty native directory.

use std::{fmt::Write as _, path::PathBuf};

use lodestone_server::world_storage::{WorldStorage, WorldStorageBackend};

const USAGE: &str = "usage: lodestone-server native-compact --native-path <native-store>\n\
Stop every server using this native store before running compaction.";

#[derive(Debug)]
struct Launch {
    native_path: PathBuf,
}

/// Parses and executes the `native-compact` subcommand.
pub(super) fn run(args: impl IntoIterator<Item = impl Into<String>>) -> Result<String, String> {
    let launch = parse(args)?;
    let segment = launch.native_path.join("world.ls");
    if !segment.is_file() {
        return Err(format!(
            "native compaction requires an existing native segment at {}; refusing to create one",
            segment.display()
        ));
    }
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: launch.native_path.clone(),
    })
    .map_err(|error| {
        format!(
            "could not open native backend {} for compaction: {error}",
            launch.native_path.display()
        )
    })?;
    let result = storage
        .compact_native()
        .map_err(|error| format!("native compaction failed: {error}"))?;
    let mut output = String::new();
    writeln!(
        output,
        "Compacted {} latest records in {}: {} bytes -> {} bytes.",
        result.records,
        launch.native_path.display(),
        result.before_bytes,
        result.after_bytes,
    )
    .expect("write to String");
    Ok(output)
}

fn parse(args: impl IntoIterator<Item = impl Into<String>>) -> Result<Launch, String> {
    let mut args = args.into_iter().map(Into::into);
    let mut native_path = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--native-path" => {
                native_path = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--native-path requires a value".to_owned())?,
                ));
            }
            "--help" | "-h" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown native-compact option {other:?}\n{USAGE}")),
        }
    }
    Ok(Launch {
        native_path: native_path.ok_or_else(|| format!("--native-path is required\n{USAGE}"))?,
    })
}

#[cfg(test)]
mod tests {
    use lodestone_server::world_storage::{NativePlayerRecord, WorldStorage, WorldStorageBackend};
    use lodestone_storage_schema::BuiltinDimension;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn command_compacts_replacements_and_reopens_the_latest_record() {
        let scratch = tempdir().expect("create isolated compaction scratch directory");
        let native_path = scratch.path().join("native");
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: native_path.clone(),
        })
        .expect("open native fixture");
        let mut player = NativePlayerRecord {
            uuid: [0x71; 16],
            dimension: BuiltinDimension::Overworld,
            x_fixed: 1,
            y_fixed: 2,
            z_fixed: 3,
            yaw_millidegrees: 4,
            pitch_millidegrees: 5,
        };
        storage
            .write_dirty_player(player)
            .expect("write original player record");
        player.x_fixed = 99;
        storage
            .write_dirty_player(player)
            .expect("write replacement player record");
        let before = std::fs::metadata(native_path.join("world.ls"))
            .expect("read append segment size before compaction")
            .len();
        drop(storage);

        let output = run([
            "--native-path",
            native_path.to_str().expect("UTF-8 native fixture path"),
        ])
        .expect("compact existing native segment");
        assert!(output.contains("Compacted 1 latest records"));
        let after = std::fs::metadata(native_path.join("world.ls"))
            .expect("read compacted segment size")
            .len();
        assert!(after < before, "compaction must reclaim the replaced append");

        let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: native_path,
        })
        .expect("reopen compacted native segment");
        assert_eq!(
            reopened.load_player(player.uuid).expect("read compacted player"),
            Some(player),
            "compaction must retain the newest typed record after a fresh open"
        );
    }

    #[test]
    fn command_refuses_a_missing_segment_without_creating_the_directory() {
        let scratch = tempdir().expect("create isolated compaction scratch directory");
        let missing = scratch.path().join("missing");
        let error = run([
            "--native-path",
            missing.to_str().expect("UTF-8 missing native path"),
        ])
        .expect_err("a typo must not create an empty native store");
        assert!(error.contains("requires an existing native segment"));
        assert!(!missing.exists(), "refusal must not create the missing path");
    }
}
