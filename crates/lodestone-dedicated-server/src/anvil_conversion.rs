//! Explicit command-line entry point for terrain-only Anvil/native conversion.
//!
//! This intentionally sits in the dedicated-server binary rather than in the
//! game launch path. Conversion is an operator action with an explicit source,
//! destination, native-store path, vertical window, and review step; it must
//! not become an implicit consequence of opening a world.

use std::{fmt, fmt::Write as _, path::PathBuf};

use lodestone_anvil::{
    CompressionScheme,
    import_preflight::{LossDecision, PreflightReport},
};
use lodestone_server::{
    anvil_world_export::{
        ChunkCoordinate, WorldExportInput, WorldExportLossDecision, WorldExportReport,
        export_world_directory, preflight_world_export,
    },
    anvil_world_import::{import_world_directory, preflight_world_directory},
    world_storage::{WorldStorage, WorldStorageBackend},
};

const USAGE: &str = concat!(
    "usage:\n",
    "  lodestone-server anvil-convert import --source <anvil-world> ",
    "--destination <native-store> --native-path <native-store> --dimension <id> ",
    "--min-y <blocks> --height <blocks> [--apply --acknowledge <review-token>]\n",
    "  lodestone-server anvil-convert export --source <native-store> ",
    "--destination <anvil-world> --native-path <native-store> --min-y <blocks> ",
    "--height <blocks> (--chunk <x,z> [--chunk <x,z> ...] | --all-terrain) --game-time <ticks> ",
    "--timestamp <seconds> --compression <gzip|zlib|uncompressed|lz4> ",
    "[--apply --acknowledge <review-token>]\n\n",
    "Without --apply this command only reports its payload-free preflight and refuses mutation. ",
    "A lossy --apply requires the exact review token printed by that preflight.",
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Direction {
    Import,
    Export,
}

impl Direction {
    const fn name(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Export => "export",
        }
    }
}

#[derive(Debug)]
struct ConversionLaunch {
    direction: Direction,
    source: PathBuf,
    destination: PathBuf,
    native_path: PathBuf,
    min_y: i32,
    height: i32,
    dimension: Option<String>,
    chunks: Vec<ChunkCoordinate>,
    all_terrain: bool,
    game_time: Option<u64>,
    timestamp: Option<u32>,
    compression: Option<CompressionScheme>,
    apply: bool,
    acknowledgement: Option<String>,
}

/// Parses and executes the `anvil-convert` subcommand.
///
/// The returned text is deliberately payload-free. It includes the typed
/// preflight's debug form and a deterministic acknowledgement token, never an
/// unsupported NBT value or an access credential.
pub(super) fn run(args: impl IntoIterator<Item = impl Into<String>>) -> Result<String, String> {
    let launch = parse(args)?;
    execute(&launch)
}

fn parse(args: impl IntoIterator<Item = impl Into<String>>) -> Result<ConversionLaunch, String> {
    let mut args = args.into_iter().map(Into::into);
    let direction = match args.next().as_deref() {
        Some("import") => Direction::Import,
        Some("export") => Direction::Export,
        Some("--help") | Some("-h") | None => return Err(USAGE.to_owned()),
        Some(other) => {
            return Err(format!(
                "anvil-convert expects import or export, got {other:?}\n{USAGE}"
            ));
        }
    };
    let mut source = None;
    let mut destination = None;
    let mut native_path = None;
    let mut min_y = None;
    let mut height = None;
    let mut dimension = None;
    let mut chunks = Vec::new();
    let mut all_terrain = false;
    let mut game_time = None;
    let mut timestamp = None;
    let mut compression = None;
    let mut apply = false;
    let mut acknowledgement = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--source" => source = Some(PathBuf::from(next_arg("--source", &mut args)?)),
            "--destination" => {
                destination = Some(PathBuf::from(next_arg("--destination", &mut args)?));
            }
            "--native-path" => {
                native_path = Some(PathBuf::from(next_arg("--native-path", &mut args)?));
            }
            "--min-y" => {
                min_y = Some(parse_number("--min-y", &next_arg("--min-y", &mut args)?)?);
            }
            "--height" => {
                height = Some(parse_number("--height", &next_arg("--height", &mut args)?)?);
            }
            "--dimension" => dimension = Some(next_arg("--dimension", &mut args)?),
            "--chunk" => chunks.push(parse_chunk(&next_arg("--chunk", &mut args)?)?),
            "--all-terrain" => all_terrain = true,
            "--game-time" => {
                game_time = Some(parse_number("--game-time", &next_arg("--game-time", &mut args)?)?);
            }
            "--timestamp" => {
                timestamp = Some(parse_number("--timestamp", &next_arg("--timestamp", &mut args)?)?);
            }
            "--compression" => {
                compression = Some(parse_compression(&next_arg("--compression", &mut args)?)?);
            }
            "--apply" => apply = true,
            "--acknowledge" => acknowledgement = Some(next_arg("--acknowledge", &mut args)?),
            "--help" | "-h" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown anvil-convert option {other:?}\n{USAGE}")),
        }
    }

    let launch = ConversionLaunch {
        direction,
        source: source.ok_or_else(|| "--source is required".to_owned())?,
        destination: destination.ok_or_else(|| "--destination is required".to_owned())?,
        native_path: native_path.ok_or_else(|| "--native-path is required".to_owned())?,
        min_y: min_y.ok_or_else(|| "--min-y is required".to_owned())?,
        height: height.ok_or_else(|| "--height is required".to_owned())?,
        dimension,
        chunks,
        all_terrain,
        game_time,
        timestamp,
        compression,
        apply,
        acknowledgement,
    };
    validate_shape(&launch)?;
    Ok(launch)
}

fn validate_shape(launch: &ConversionLaunch) -> Result<(), String> {
    if launch.source == launch.destination {
        return Err("--source and --destination must name different paths".to_owned());
    }
    let native_endpoint = match launch.direction {
        Direction::Import => &launch.destination,
        Direction::Export => &launch.source,
    };
    if native_endpoint != &launch.native_path {
        return Err(format!(
            "--native-path must exactly name the {} endpoint for {}",
            if launch.direction == Direction::Import { "destination" } else { "source" },
            launch.direction.name(),
        ));
    }
    match launch.direction {
        Direction::Import => {
            if launch.dimension.as_deref().is_none_or(str::is_empty) {
                return Err("import requires a non-empty --dimension".to_owned());
            }
            if !launch.chunks.is_empty()
                || launch.all_terrain
                || launch.game_time.is_some()
                || launch.timestamp.is_some()
                || launch.compression.is_some()
            {
                return Err("--chunk, --game-time, --timestamp, and --compression are export-only".to_owned());
            }
        }
        Direction::Export => {
            if launch.dimension.is_some() {
                return Err("--dimension is import-only; export is terrain-only".to_owned());
            }
            if launch.all_terrain && !launch.chunks.is_empty() {
                return Err("export accepts either --all-terrain or explicit --chunk values, not both".to_owned());
            }
            if !launch.all_terrain && launch.chunks.is_empty() {
                return Err("export requires --all-terrain or at least one explicit --chunk <x,z>".to_owned());
            }
            if launch.game_time.is_none()
                || launch.timestamp.is_none()
                || launch.compression.is_none()
            {
                return Err("export requires --game-time, --timestamp, and --compression".to_owned());
            }
        }
    }
    Ok(())
}

fn execute(launch: &ConversionLaunch) -> Result<String, String> {
    match launch.direction {
        // Import preflight reads only the Anvil source. Opening a destination
        // NativeStore may create its directory, so defer it until --apply has
        // passed every review gate.
        Direction::Import => execute_import(launch),
        Direction::Export => {
            let storage = open_native_backend(launch)?;
            execute_export(launch, &storage)
        }
    }
}

fn open_native_backend(launch: &ConversionLaunch) -> Result<WorldStorage, String> {
    WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: launch.native_path.clone(),
    })
    .map_err(|error| {
        format!(
            "could not open native backend {}: {error}",
            launch.native_path.display()
        )
    })
}

fn execute_import(launch: &ConversionLaunch) -> Result<String, String> {
    let dimension = launch.dimension.as_deref().expect("validated import dimension");
    let report = preflight_world_directory(dimension, &launch.source)
        .map_err(|error| format!("import preflight failed: {error}"))?;
    let token = review_token(launch, &report);
    let mut output = format_review(Direction::Import, &report, &token);
    require_apply(launch, &report, &token, &mut output)?;
    let storage = open_native_backend(launch)?;
    let result = import_world_directory(
        &storage,
        dimension,
        &launch.source,
        launch.min_y,
        launch.height,
        Some(report.decide(LossDecision::ProceedAndDiscardUnsupported)),
    )
    .map_err(|error| format!("import refused before completing conversion: {error}"))?;
    writeln!(
        output,
        "Converted {} terrain chunks into {}.",
        result.records_written,
        launch.destination.display()
    )
    .expect("write to String");
    Ok(output)
}

fn execute_export(launch: &ConversionLaunch, storage: &WorldStorage) -> Result<String, String> {
    let chunks = if launch.all_terrain {
        storage
            .native_chunk_coordinates()
            .map_err(|error| format!("could not enumerate committed native terrain: {error}"))?
            .into_iter()
            .map(|coordinate| ChunkCoordinate {
                x: coordinate.column_x,
                z: coordinate.column_z,
            })
            .collect()
    } else {
        launch.chunks.clone()
    };
    let selected_count = chunks.len();
    let input = WorldExportInput::new(
        chunks,
        launch.min_y,
        launch.height,
        launch.game_time.expect("validated export game time"),
        launch.compression.expect("validated export compression"),
        launch.timestamp.expect("validated export timestamp"),
    )
    .map_err(|error| format!("invalid export selection: {error}"))?;
    let report = preflight_world_export(storage, &input)
        .map_err(|error| format!("export preflight failed: {error}"))?;
    let token = review_token(launch, &report);
    let mut output = format!(
        "Selected {selected_count} terrain chunks.\n{}",
        format_review(Direction::Export, &report, &token)
    );
    require_apply(launch, &report, &token, &mut output)?;
    let result = export_world_directory(
        storage,
        &input,
        &launch.destination,
        Some(report.decide(WorldExportLossDecision::ProceedAndDiscardUnsupported)),
    )
    .map_err(|error| format!("export refused before publishing conversion: {error}"))?;
    writeln!(
        output,
        "Converted {} terrain chunks into {}.",
        result.chunks_exported,
        launch.destination.display()
    )
    .expect("write to String");
    Ok(output)
}

trait ReviewReport: fmt::Debug {
    fn has_loss(&self) -> bool;
    fn has_blocker(&self) -> bool;
}

impl ReviewReport for PreflightReport {
    fn has_loss(&self) -> bool {
        !self.unsupported().is_empty()
    }

    fn has_blocker(&self) -> bool {
        !self.blockers().is_empty()
    }
}

impl ReviewReport for WorldExportReport {
    fn has_loss(&self) -> bool {
        self.unsupported_count() != 0
    }

    fn has_blocker(&self) -> bool {
        false
    }
}

fn require_apply<T: ReviewReport>(
    launch: &ConversionLaunch,
    report: &T,
    token: &str,
    output: &mut String,
) -> Result<(), String> {
    if !launch.apply {
        writeln!(output, "Refusing mutation without --apply.").expect("write to String");
        return Err(output.clone());
    }
    if report.has_blocker() {
        return Err(format!(
            "{output}Preflight contains blocking data; no acknowledgement can authorize it."
        ));
    }
    if report.has_loss() && launch.acknowledgement.as_deref() != Some(token) {
        return Err(format!("{output}Lossy conversion requires --acknowledge {token}."));
    }
    Ok(())
}

fn format_review<T: fmt::Debug>(direction: Direction, report: &T, token: &str) -> String {
    format!(
        "{} preflight (payload-free):\n{report:#?}\nReview token: {token}\n",
        direction.name()
    )
}

fn review_token<T: fmt::Debug>(launch: &ConversionLaunch, report: &T) -> String {
    let reviewed = format!(
        "v2|{:?}|{}|{}|{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
        launch.direction,
        launch.source.display(),
        launch.destination.display(),
        launch.native_path.display(),
        launch.min_y,
        launch.height,
        launch.dimension,
        launch.chunks,
        launch.all_terrain,
        launch.game_time,
        launch.timestamp,
        report,
    );
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in reviewed.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("anvil-review-v1-{hash:016x}")
}

fn parse_number<T: std::str::FromStr>(flag: &str, value: &str) -> Result<T, String> {
    value.parse().map_err(|_| format!("{flag} has invalid value {value:?}"))
}

fn next_arg(args_name: &str, args: &mut impl Iterator<Item = String>) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{args_name} requires a value"))
}

fn parse_chunk(value: &str) -> Result<ChunkCoordinate, String> {
    let Some((x, z)) = value.split_once(',') else {
        return Err(format!("--chunk must be x,z, got {value:?}"));
    };
    Ok(ChunkCoordinate {
        x: parse_number("--chunk x", x)?,
        z: parse_number("--chunk z", z)?,
    })
}

fn parse_compression(value: &str) -> Result<CompressionScheme, String> {
    match value {
        "gzip" => Ok(CompressionScheme::Gzip),
        "zlib" => Ok(CompressionScheme::Zlib),
        "uncompressed" => Ok(CompressionScheme::Uncompressed),
        "lz4" => Ok(CompressionScheme::Lz4),
        _ => Err(format!(
            "--compression must be gzip, zlib, uncompressed, or lz4; got {value:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use lodestone_server::{
        ChunkColumn, ScheduledTickHandle, TickPriority,
        world_storage::{NativeDirtyChunkRecord, WorldStorage, WorldStorageBackend},
    };

    use super::*;

    const SCRATCH: &str = "/private/tmp/lodestone-wave-storage-enum-711";

    struct Scratch;

    impl Scratch {
        fn create() -> Self {
            let path = Path::new(SCRATCH);
            assert!(
                !path.exists(),
                "shared CLI scratch path must be absent before this test"
            );
            std::fs::create_dir(path).expect("create exact CLI scratch path");
            Self
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(SCRATCH).expect("remove exact CLI scratch path");
        }
    }

    fn token(output: &str) -> &str {
        output
            .lines()
            .find_map(|line| line.strip_prefix("Review token: "))
            .expect("preflight prints a review token")
    }

    #[test]
    fn command_preflights_then_drives_export_and_import_coordinators() {
        let _scratch = Scratch::create();
        let root = Path::new(SCRATCH);
        let native_source = root.join("native-source");
        let anvil_destination = root.join("anvil-destination");
        let native_destination = root.join("native-destination");
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: native_source.clone(),
        })
        .expect("open fixture native backend");
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(1, 1, 2, "minecraft:diamond_block");
        let light = lodestone_world::ColumnLight::new(column.section_count());
        let scheduled = ScheduledTickHandle::new();
        scheduled.with(|queues| {
            assert!(queues.fluid.schedule(
                (1, 1, 2),
                lodestone_server::fluid::TICK_FLUID.to_owned(),
                5,
                TickPriority::Normal,
            ));
        });
        storage
            .write_dirty_chunk(NativeDirtyChunkRecord::new(0, 0, &column, &light, &scheduled))
            .expect("write complete typed native fixture chunk");
        storage
            .write_dirty_chunk(NativeDirtyChunkRecord::new(-1, 0, &column, &light, &scheduled))
            .expect("write a second complete typed native fixture chunk");
        drop(storage);

        let export = [
            "export", "--source", native_source.to_str().unwrap(), "--destination",
            anvil_destination.to_str().unwrap(), "--native-path",
            native_source.to_str().unwrap(), "--min-y", "0", "--height", "16", "--all-terrain",
            "--game-time", "0", "--timestamp", "1", "--compression", "zlib",
        ];
        let preview = run(export).expect_err("default export reports and refuses mutation");
        assert!(
            preview.contains("Selected 2 terrain chunks."),
            "all-terrain preview reports its exact recovered-index selection"
        );
        assert!(preview.contains("Refusing mutation without --apply."));
        assert!(!anvil_destination.exists(), "preview cannot publish a destination");

        let mut unacknowledged_export = export.to_vec();
        unacknowledged_export.push("--apply");
        let refusal = run(unacknowledged_export)
            .expect_err("lossy export cannot apply without the reviewed token");
        assert!(refusal.contains("Lossy conversion requires --acknowledge"));
        assert!(
            !anvil_destination.exists(),
            "an unacknowledged loss cannot publish a destination"
        );
        let mut apply_export = export.to_vec();
        apply_export.extend(["--apply", "--acknowledge", token(&preview)]);
        run(apply_export).expect("acknowledged export drives the real coordinator");
        assert!(anvil_destination.join("region/r.0.0.mca").is_file());

        let import = [
            "import", "--source", anvil_destination.to_str().unwrap(), "--destination",
            native_destination.to_str().unwrap(), "--native-path",
            native_destination.to_str().unwrap(), "--dimension", "minecraft:overworld", "--min-y",
            "0", "--height", "16",
        ];
        let preview = run(import).expect_err("default import reports and refuses mutation");
        assert!(preview.contains("Refusing mutation without --apply."));
        assert!(!native_destination.exists(), "preview cannot create a native backend");
        let mut apply_import = import.to_vec();
        apply_import.push("--apply");
        let token = token(&preview).to_owned();
        apply_import.extend(["--acknowledge", &token]);
        run(apply_import).expect("acknowledged import drives the real coordinator");
        let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: native_destination,
        })
        .expect("open imported native backend");
        assert!(
            reopened
                .load_chunk(0, 0, 0, 16)
                .expect("read imported chunk")
                .is_some()
        );
        assert!(
            reopened
                .load_chunk(-1, 0, 0, 16)
                .expect("read second imported chunk")
                .is_some()
        );
    }

    #[test]
    fn parser_rejects_native_endpoint_mismatch_and_ambiguous_destination() {
        let mismatch = parse([
            "import", "--source", "/tmp/anvil", "--destination", "/tmp/native-a",
            "--native-path", "/tmp/native-b", "--dimension", "minecraft:overworld", "--min-y",
            "0", "--height", "16",
        ])
        .expect_err("native backend must name the import destination");
        assert!(mismatch.contains("--native-path"));
        let ambiguous = parse([
            "export", "--source", "/tmp/native", "--destination", "/tmp/native",
            "--native-path", "/tmp/native", "--min-y", "0", "--height", "16", "--chunk",
            "0,0", "--game-time", "0", "--timestamp", "1", "--compression", "zlib",
        ])
        .expect_err("conversion endpoints must not alias");
        assert!(ambiguous.contains("different paths"));
        let explicit = parse([
            "export", "--source", "/tmp/native", "--destination", "/tmp/anvil",
            "--native-path", "/tmp/native", "--min-y", "0", "--height", "16", "--chunk",
            "0,0", "--game-time", "0", "--timestamp", "1", "--compression", "zlib",
        ])
        .expect("an explicit chunk selection remains supported");
        assert!(!explicit.all_terrain);
        assert_eq!(explicit.chunks, [ChunkCoordinate { x: 0, z: 0 }]);
        let mixed = parse([
            "export", "--source", "/tmp/native", "--destination", "/tmp/anvil",
            "--native-path", "/tmp/native", "--min-y", "0", "--height", "16", "--chunk",
            "0,0", "--all-terrain", "--game-time", "0", "--timestamp", "1",
            "--compression", "zlib",
        ])
        .expect_err("selection modes must remain unambiguous");
        assert!(mixed.contains("either --all-terrain or explicit --chunk"));
    }
}
