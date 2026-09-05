//! Explicit command-line entry point for bounded Anvil/native conversion.
//!
//! This intentionally sits in the dedicated-server binary rather than in the
//! game launch path. Conversion is an operator action with an explicit source,
//! destination, native-store path, vertical window, and review step; it must
//! not become an implicit consequence of opening a world.

use std::{fmt, fmt::Write as _, path::PathBuf};

use lodestone_anvil::{
    CompressionScheme,
    import_preflight::{LossDecision, PreflightReport},
    level_dat, world_gen_settings,
};
use lodestone_server::{
    anvil_player_export::export_all_players,
    anvil_native_entity_import::{
        EntityChunkSelection, EntityLossDecision, SelectedEntityChunk, discover_entity_chunks,
        import_entity_batch, preflight_entity_batch,
    },
    anvil_player_storage::{
        PlayerBatchImportReport, PlayerFileSelection, PlayerLossDecision, discover_player_files,
        import_player_batch, preflight_player_batch,
    },
    anvil_import::{import_world_properties, preflight_world_properties},
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
    "  lodestone-server anvil-convert import-metadata --source <anvil-world> ",
    "--destination <native-store> --native-path <native-store> ",
    "[--apply --acknowledge <review-token>]\n\n",
    "  lodestone-server anvil-convert import-players --source <anvil-world> ",
    "--destination <native-store> --native-path <native-store> ",
    "(--player <uuid> [--player <uuid> ...] | --all-players) ",
    "[--apply --acknowledge <review-token>]\n\n",
    "  lodestone-server anvil-convert import-entities --source <anvil-world> ",
    "--destination <native-store> --native-path <native-store> --min-y <blocks> ",
    "--height <blocks> (--entity-chunk <x,z> [--entity-chunk <x,z> ...] | --all-entities) ",
    "[--apply --acknowledge <review-token>]\n\n",
    "  lodestone-server anvil-convert export-players --source <native-store> ",
    "--destination <anvil-world> --native-path <native-store> --apply\n\n",
    "Without --apply this command only reports its payload-free preflight and refuses mutation. ",
    "A lossy --apply requires the exact review token printed by that preflight.",
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Direction {
    ImportTerrain,
    ExportTerrain,
    ImportMetadata,
    ImportPlayers,
    ImportEntities,
    ExportPlayers,
}

impl Direction {
    const fn name(self) -> &'static str {
        match self {
            Self::ImportTerrain => "import",
            Self::ExportTerrain => "export",
            Self::ImportMetadata => "import-metadata",
            Self::ImportPlayers => "import-players",
            Self::ImportEntities => "import-entities",
            Self::ExportPlayers => "export-players",
        }
    }
}

#[derive(Debug)]
struct ConversionLaunch {
    direction: Direction,
    source: PathBuf,
    destination: PathBuf,
    native_path: PathBuf,
    min_y: Option<i32>,
    height: Option<i32>,
    dimension: Option<String>,
    chunks: Vec<ChunkCoordinate>,
    all_terrain: bool,
    game_time: Option<u64>,
    timestamp: Option<u32>,
    compression: Option<CompressionScheme>,
    players: Vec<uuid::Uuid>,
    all_players: bool,
    entity_chunks: Vec<SelectedEntityChunk>,
    all_entities: bool,
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
        Some("import") => Direction::ImportTerrain,
        Some("export") => Direction::ExportTerrain,
        Some("import-metadata") => Direction::ImportMetadata,
        Some("import-players") => Direction::ImportPlayers,
        Some("import-entities") => Direction::ImportEntities,
        Some("export-players") => Direction::ExportPlayers,
        Some("--help") | Some("-h") | None => return Err(USAGE.to_owned()),
        Some(other) => {
            return Err(format!(
                "anvil-convert expects import, export, import-metadata, import-players, import-entities, or export-players, got {other:?}\n{USAGE}"
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
    let mut players = Vec::new();
    let mut all_players = false;
    let mut entity_chunks = Vec::new();
    let mut all_entities = false;
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
            "--player" => players.push(
                next_arg("--player", &mut args)?
                    .parse()
                    .map_err(|_| "--player must be a canonical UUID".to_owned())?,
            ),
            "--all-players" => all_players = true,
            "--entity-chunk" => {
                let chunk = parse_chunk(&next_arg("--entity-chunk", &mut args)?)?;
                entity_chunks.push(SelectedEntityChunk {
                    column_x: chunk.x,
                    column_z: chunk.z,
                });
            }
            "--all-entities" => all_entities = true,
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
        min_y,
        height,
        dimension,
        chunks,
        all_terrain,
        game_time,
        timestamp,
        compression,
        players,
        all_players,
        entity_chunks,
        all_entities,
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
        Direction::ImportTerrain
        | Direction::ImportMetadata
        | Direction::ImportPlayers
        | Direction::ImportEntities => {
            &launch.destination
        }
        Direction::ExportTerrain | Direction::ExportPlayers => &launch.source,
    };
    if native_endpoint != &launch.native_path {
        return Err(format!(
            "--native-path must exactly name the {} endpoint for {}",
            if matches!(
                launch.direction,
                Direction::ImportTerrain
                    | Direction::ImportMetadata
                    | Direction::ImportPlayers
                    | Direction::ImportEntities
            ) {
                "destination"
            } else {
                "source"
            },
            launch.direction.name(),
        ));
    }
    match launch.direction {
        Direction::ImportTerrain => {
            if launch.min_y.is_none() || launch.height.is_none() {
                return Err("import requires --min-y and --height".to_owned());
            }
            if launch.dimension.as_deref().is_none_or(str::is_empty) {
                return Err("import requires a non-empty --dimension".to_owned());
            }
            if !launch.chunks.is_empty()
                || launch.all_terrain
                || launch.game_time.is_some()
                || launch.timestamp.is_some()
                || launch.compression.is_some()
                || !launch.players.is_empty()
                || launch.all_players
            {
                return Err("--chunk, --game-time, --timestamp, and --compression are export-only".to_owned());
            }
        }
        Direction::ExportTerrain => {
            if launch.min_y.is_none() || launch.height.is_none() {
                return Err("export requires --min-y and --height".to_owned());
            }
            if launch.dimension.is_some() {
                return Err("--dimension is import-only; export is terrain-only".to_owned());
            }
            if !launch.players.is_empty() || launch.all_players {
                return Err("--player and --all-players are import-players-only".to_owned());
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
        Direction::ImportMetadata => {
            if launch.min_y.is_some()
                || launch.height.is_some()
                || launch.dimension.is_some()
                || !launch.chunks.is_empty()
                || launch.all_terrain
                || launch.game_time.is_some()
                || launch.timestamp.is_some()
                || launch.compression.is_some()
                || !launch.players.is_empty()
                || launch.all_players
            {
                return Err(
                    "import-metadata accepts only --source, --destination, --native-path, --apply, and --acknowledge"
                        .to_owned(),
                );
            }
        }
        Direction::ImportPlayers => {
            if launch.min_y.is_some()
                || launch.height.is_some()
                || launch.dimension.is_some()
                || !launch.chunks.is_empty()
                || launch.all_terrain
                || launch.game_time.is_some()
                || launch.timestamp.is_some()
                || launch.compression.is_some()
            {
                return Err(
                    "import-players accepts only --source, --destination, --native-path, --player, --all-players, --apply, and --acknowledge"
                        .to_owned(),
                );
            }
            if launch.all_players == !launch.players.is_empty() {
                return Err(
                    "import-players requires exactly one selection mode: --all-players or one or more --player <uuid>"
                        .to_owned(),
                );
            }
        }
        Direction::ImportEntities => {
            if launch.min_y.is_none() || launch.height.is_none() {
                return Err("import-entities requires --min-y and --height".to_owned());
            }
            if launch.dimension.is_some()
                || !launch.chunks.is_empty()
                || launch.all_terrain
                || launch.game_time.is_some()
                || launch.timestamp.is_some()
                || launch.compression.is_some()
                || !launch.players.is_empty()
                || launch.all_players
            {
                return Err(
                    "import-entities accepts only --source, --destination, --native-path, --min-y, --height, --entity-chunk, --all-entities, --apply, and --acknowledge"
                        .to_owned(),
                );
            }
            if launch.all_entities == !launch.entity_chunks.is_empty() {
                return Err(
                    "import-entities requires exactly one selection mode: --all-entities or one or more --entity-chunk <x,z>"
                        .to_owned(),
                );
            }
        }
        Direction::ExportPlayers => {
            if launch.min_y.is_some()
                || launch.height.is_some()
                || launch.dimension.is_some()
                || !launch.chunks.is_empty()
                || launch.all_terrain
                || launch.game_time.is_some()
                || launch.timestamp.is_some()
                || launch.compression.is_some()
                || !launch.players.is_empty()
                || launch.all_players
                || !launch.entity_chunks.is_empty()
                || launch.all_entities
                || launch.acknowledgement.is_some()
            {
                return Err(
                    "export-players accepts only --source, --destination, --native-path, and --apply"
                        .to_owned(),
                );
            }
            if !launch.apply {
                return Err(
                    "export-players requires --apply; it is lossless but mutates the Anvil destination"
                        .to_owned(),
                );
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
        Direction::ImportTerrain => execute_import(launch),
        Direction::ImportMetadata => execute_metadata_import(launch),
        Direction::ImportPlayers => execute_player_import(launch),
        Direction::ImportEntities => execute_entity_import(launch),
        Direction::ExportPlayers => {
            let storage = open_native_backend(launch)?;
            let result = export_all_players(&storage, &launch.destination)
                .map_err(|error| format!("player export failed: {error}"))?;
            Ok(format!(
                "Converted {} typed native players into {}.\n",
                result.players_exported,
                launch.destination.display()
            ))
        }
        Direction::ExportTerrain => {
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
    let mut output = format_review(Direction::ImportTerrain, &report, &token);
    require_apply(launch, &report, &token, &mut output)?;
    let storage = open_native_backend(launch)?;
    let result = import_world_directory(
        &storage,
        dimension,
        &launch.source,
        launch.min_y.expect("validated import min Y"),
        launch.height.expect("validated import height"),
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

fn execute_metadata_import(launch: &ConversionLaunch) -> Result<String, String> {
    let level = level_dat::read_from_file(&level_dat::path_in(&launch.source))
        .map_err(|error| format!("metadata preflight could not read level.dat: {error}"))?;
    let settings =
        world_gen_settings::read_from_file(&world_gen_settings::path_in(&launch.source)).map_err(
            |error| format!("metadata preflight could not read world-generation settings: {error}"),
        )?;
    let report = preflight_world_properties(&level, &settings);
    let token = review_token(launch, &report);
    let mut output = format_review(Direction::ImportMetadata, &report, &token);
    require_apply(launch, &report, &token, &mut output)?;
    let storage = open_native_backend(launch)?;
    let records_written = import_world_properties(
        &storage,
        &level,
        &settings,
        Some(report.decide(LossDecision::ProceedAndDiscardUnsupported)),
    )
    .map_err(|error| format!("metadata import refused before completing conversion: {error}"))?;
    writeln!(
        output,
        "Converted {records_written} typed world-properties record into {}.",
        launch.destination.display()
    )
    .expect("write to String");
    Ok(output)
}

fn execute_player_import(launch: &ConversionLaunch) -> Result<String, String> {
    let selection = if launch.all_players {
        PlayerFileSelection::All
    } else {
        PlayerFileSelection::Uuids(launch.players.clone())
    };
    let selected = discover_player_files(&launch.source, selection)
        .map_err(|error| format!("player discovery failed: {error}"))?;
    let plan = preflight_player_batch(&selected)
        .map_err(|error| format!("player preflight failed: {error}"))?;
    let token = review_token(launch, plan.report());
    let mut output = format!(
        "Selected {} player files.\n{}",
        selected.len(),
        format_review(Direction::ImportPlayers, plan.report(), &token)
    );
    require_apply(launch, plan.report(), &token, &mut output)?;
    let authorization = plan
        .report()
        .decide(PlayerLossDecision::ProceedAndDiscardUnsupported);
    let storage = open_native_backend(launch)?;
    let result = import_player_batch(&storage, plan, Some(authorization))
        .map_err(|error| format!("player import refused before committing conversion: {error}"))?;
    drop(storage);

    let reopened = open_native_backend(launch)?;
    for player in result.report.players() {
        if reopened
            .load_player(*player.uuid.as_bytes())
            .map_err(|error| format!("player import reopen failed: {error}"))?
            .is_none()
        {
            return Err(format!(
                "player import reopen found no locator for selected UUID {}",
                player.uuid
            ));
        }
    }
    writeln!(
        output,
        "Converted {} player locators into {} and reopened every selected locator.",
        result.records_written,
        launch.destination.display()
    )
    .expect("write to String");
    Ok(output)
}

fn execute_entity_import(launch: &ConversionLaunch) -> Result<String, String> {
    let selection = if launch.all_entities {
        EntityChunkSelection::All
    } else {
        EntityChunkSelection::Chunks(launch.entity_chunks.clone())
    };
    let selected = discover_entity_chunks(&launch.source, selection)
        .map_err(|error| format!("entity discovery failed: {error}"))?;
    let plan = preflight_entity_batch(
        &launch.source,
        &selected,
        launch.min_y.expect("validated entity import min Y"),
        launch.height.expect("validated entity import height"),
    )
    .map_err(|error| format!("entity preflight failed: {error}"))?;
    let token = review_token(launch, plan.report());
    let mut output = format!(
        "Selected {} entity sidecar chunks.\n{}",
        selected.len(),
        format_review(Direction::ImportEntities, plan.report(), &token)
    );
    require_apply(launch, plan.report(), &token, &mut output)?;
    let authorization = plan
        .report()
        .decide(EntityLossDecision::ProceedAndDiscardUnsupported);
    let storage = open_native_backend(launch)?;
    let result = import_entity_batch(&storage, plan, Some(authorization))
        .map_err(|error| format!("entity import refused before committing conversion: {error}"))?;
    drop(storage);

    let reopened = open_native_backend(launch)?;
    for chunk in result.report.chunks() {
        let sidecar = crate_entity_uuids(&launch.source, chunk.column_x, chunk.column_z)?;
        for uuid in sidecar {
            if reopened
                .load_entity(
                    *uuid.as_bytes(),
                    chunk.column_x,
                    chunk.column_z,
                    launch.min_y.expect("validated entity import min Y"),
                    launch.height.expect("validated entity import height"),
                )
                .map_err(|error| format!("entity import reopen failed: {error}"))?
                .is_none()
            {
                return Err(format!(
                    "entity import reopen found no pose for selected UUID {uuid}"
                ));
            }
        }
    }
    writeln!(
        output,
        "Converted {} resident entity poses from {} sidecar chunks into {} and reopened every selected pose.",
        result.records_written,
        result.chunks_seen,
        launch.destination.display()
    )
    .expect("write to String");
    Ok(output)
}

fn crate_entity_uuids(source: &std::path::Path, column_x: i32, column_z: i32) -> Result<Vec<uuid::Uuid>, String> {
    lodestone_server::entity_storage::EntityStorage::open_readonly(source)
        .load_chunk(column_x, column_z)
        .map_err(|error| format!("entity import reopen could not reread source sidecar: {error}"))
        .map(|entities| entities.into_iter().map(|entity| entity.uuid).collect())
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
        launch.min_y.expect("validated export min Y"),
        launch.height.expect("validated export height"),
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
        format_review(Direction::ExportTerrain, &report, &token)
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

impl ReviewReport for PlayerBatchImportReport {
    fn has_loss(&self) -> bool {
        self.unsupported_count() != 0
    }

    fn has_blocker(&self) -> bool {
        self.blocker_count() != 0
    }
}

impl ReviewReport for lodestone_server::anvil_native_entity_import::EntityBatchImportReport {
    fn has_loss(&self) -> bool {
        self.unsupported_count() != 0
    }

    fn has_blocker(&self) -> bool {
        self.blocker_count() != 0
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
        "v2|{:?}|{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
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
        launch.players,
        launch.all_players,
        launch.entity_chunks,
        launch.all_entities,
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
        world_storage::{
            NativeDirtyChunkRecord, NativePlayerData, NativePlayerRecord, WorldStorage,
            WorldStorageBackend,
        },
    };

    use super::*;

    const SCRATCH: &str = "/private/tmp/lodestone-wave-storage-enum-711";
    const METADATA_SCRATCH: &str = "/private/tmp/lodestone-wave-storage-meta-711";
    const PLAYER_SCRATCH: &str = "/private/tmp/lodestone-wave-storage-player-711";
    const ENTITY_SCRATCH: &str = "/private/tmp/lodestone-wave-storage-entity-711";
    const LEVEL_DAT_FIXTURE: &[u8] =
        include_bytes!("../../lodestone-anvil/tests/support/level_dat_26_2_vanilla.dat");
    const WORLD_GEN_FIXTURE: &[u8] =
        include_bytes!("../../lodestone-anvil/tests/support/world_gen_settings_26_2_vanilla.dat");

    struct Scratch;

    struct MetadataScratch;

    struct PlayerScratch;

    struct EntityScratch;

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

    impl MetadataScratch {
        fn create() -> Self {
            let path = Path::new(METADATA_SCRATCH);
            assert!(
                !path.exists(),
                "metadata CLI scratch path must be absent before this test"
            );
            std::fs::create_dir(path).expect("create exact metadata CLI scratch path");
            Self
        }
    }

    impl Drop for MetadataScratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(METADATA_SCRATCH)
                .expect("remove exact metadata CLI scratch path");
        }
    }

    impl PlayerScratch {
        fn create() -> Self {
            let path = Path::new(PLAYER_SCRATCH);
            assert!(
                !path.exists(),
                "player CLI scratch path must be absent before this test"
            );
            std::fs::create_dir(path).expect("create exact player CLI scratch path");
            Self
        }
    }

    impl Drop for PlayerScratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(PLAYER_SCRATCH)
                .expect("remove exact player CLI scratch path");
        }
    }

    impl EntityScratch {
        fn create() -> Self {
            let path = Path::new(ENTITY_SCRATCH);
            assert!(
                !path.exists(),
                "entity CLI scratch path must be absent before this test"
            );
            std::fs::create_dir(path).expect("create exact entity CLI scratch path");
            Self
        }
    }

    impl Drop for EntityScratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(ENTITY_SCRATCH)
                .expect("remove exact entity CLI scratch path");
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

    #[test]
    fn player_command_discovers_preflights_authorizes_and_reopens_one_batch() {
        let _scratch = PlayerScratch::create();
        let root = Path::new(PLAYER_SCRATCH);
        let source = root.join("anvil-source");
        let native_destination = root.join("native-destination");
        let first: uuid::Uuid = "00000000-0000-0002-0000-000000000002"
            .parse()
            .expect("canonical UUID");
        let second: uuid::Uuid = "00000000-0000-0001-0000-000000000001"
            .parse()
            .expect("canonical UUID");
        let player = lodestone_server::player_data::PlayerData::default();
        let player_root = player.to_nbt().expect("encode supported player fixture");
        let rejected_source = root.join("rejected-anvil-source");
        let rejected_native = root.join("rejected-native-destination");
        let valid_before_corrupt: uuid::Uuid = "00000000-0000-0001-0000-000000000003"
            .parse()
            .expect("canonical UUID");
        let corrupt_after_valid: uuid::Uuid = "00000000-0000-0002-0000-000000000004"
            .parse()
            .expect("canonical UUID");
        lodestone_anvil::player_dat::write_to_file(
            &player_root,
            &lodestone_anvil::player_dat::path_in(
                &rejected_source,
                &valid_before_corrupt.to_string(),
            ),
        )
        .expect("write first selected player before corrupt fixture");
        let corrupt_path = lodestone_anvil::player_dat::path_in(
            &rejected_source,
            &corrupt_after_valid.to_string(),
        );
        std::fs::write(&corrupt_path, b"not a gzip player file")
            .expect("write later selected corrupt player fixture");
        assert!(
            run([
                "import-players",
                "--source",
                rejected_source.to_str().expect("UTF-8 rejected source"),
                "--destination",
                rejected_native.to_str().expect("UTF-8 rejected destination"),
                "--native-path",
                rejected_native.to_str().expect("UTF-8 rejected native path"),
                "--all-players",
                "--apply",
            ])
            .expect_err("every selected player must preflight before native storage opens")
            .contains("player preflight failed")
        );
        assert!(
            !rejected_native.exists(),
            "a later selected corrupt player cannot leave earlier locators committed"
        );
        for uuid in [first, second] {
            let path = lodestone_anvil::player_dat::path_in(&source, &uuid.to_string());
            lodestone_anvil::player_dat::write_to_file(&player_root, &path)
                .expect("write selected Anvil player fixture");
        }
        let first_bytes = std::fs::read(lodestone_anvil::player_dat::path_in(
            &source,
            &first.to_string(),
        ))
        .expect("capture source player before conversion");

        let command = [
            "import-players",
            "--source",
            source.to_str().expect("UTF-8 Anvil source"),
            "--destination",
            native_destination.to_str().expect("UTF-8 native destination"),
            "--native-path",
            native_destination.to_str().expect("UTF-8 native path"),
            "--all-players",
        ];
        let preview = run(command).expect_err("player preview reports and refuses mutation");
        assert!(preview.contains("Selected 2 player files."));
        assert!(preview.contains("Refusing mutation without --apply."));
        assert!(
            !native_destination.exists(),
            "all selected players preflight before opening the native destination"
        );

        let mut unacknowledged = command.to_vec();
        unacknowledged.push("--apply");
        assert!(
            run(unacknowledged)
                .expect_err("lossy locators require the exact player review token")
                .contains("Lossy conversion requires --acknowledge")
        );
        assert!(
            !native_destination.exists(),
            "an unacknowledged player batch cannot create a native destination"
        );

        let mut approved = command.to_vec();
        approved.extend(["--apply", "--acknowledge", token(&preview)]);
        let applied = run(approved).expect("acknowledged player import commits one typed batch");
        assert!(applied.contains("Converted 2 player locators"));
        assert!(applied.contains("reopened every selected locator"));
        assert_eq!(
            std::fs::read(lodestone_anvil::player_dat::path_in(&source, &first.to_string()))
                .expect("re-read source player after conversion"),
            first_bytes,
            "native import never rewrites complete Anvil player data"
        );
    }

    #[test]
    fn entity_command_preflights_authorizes_commits_and_reopens_one_batch() {
        let _scratch = EntityScratch::create();
        let root = Path::new(ENTITY_SCRATCH);
        let source = root.join("anvil-source");
        let native_destination = root.join("native-destination");
        let sidecar = lodestone_server::entity_storage::EntityStorage::new(&source)
            .expect("create fixture entity sidecar");
        let first: uuid::Uuid = "00000000-0000-0001-0000-000000000011"
            .parse()
            .expect("canonical first entity UUID");
        let second: uuid::Uuid = "00000000-0000-0002-0000-000000000012"
            .parse()
            .expect("canonical second entity UUID");
        let entity = |uuid, x, z| lodestone_server::entity_storage::SavedEntity {
            id: "minecraft:cow".parse().expect("canonical entity type"),
            uuid,
            pos: lodestone_model::Vec3::new(x, 64.0, z),
            motion: lodestone_model::Vec3::new(0.0, 0.0, 0.0),
            rotation: lodestone_model::Rotation::new(0.0, 0.0),
            health: None,
            item: None,
            age: None,
            pickup_delay: None,
            extra: Vec::new(),
        };
        sidecar
            .save(&[entity(first, 0.5, 0.5), entity(second, 16.5, 0.5)])
            .expect("write two entity sidecar chunks");
        let source_before = std::fs::read(
            source.join("dimensions/minecraft/overworld/entities/r.0.0.mca"),
        )
        .expect("capture source sidecar before conversion");

        let command = [
            "import-entities",
            "--source",
            source.to_str().expect("UTF-8 Anvil source"),
            "--destination",
            native_destination.to_str().expect("UTF-8 native destination"),
            "--native-path",
            native_destination.to_str().expect("UTF-8 native path"),
            "--min-y",
            "0",
            "--height",
            "128",
            "--all-entities",
        ];
        let preview = run(command).expect_err("entity preview reports and refuses mutation");
        assert!(preview.contains("Selected 2 entity sidecar chunks."));
        assert!(preview.contains("Motion"), "motion loss is visible before apply");
        assert!(!native_destination.exists(), "preview cannot create native storage");
        let mut unacknowledged = command.to_vec();
        unacknowledged.push("--apply");
        assert!(
            run(unacknowledged)
                .expect_err("lossy entity conversion needs its exact token")
                .contains("Lossy conversion requires --acknowledge")
        );
        assert!(
            !native_destination.exists(),
            "unacknowledged entity loss cannot create native storage"
        );
        let mut approved = command.to_vec();
        approved.extend(["--apply", "--acknowledge", token(&preview)]);
        let applied = run(approved).expect("acknowledged entity batch commits");
        assert!(applied.contains("Converted 2 resident entity poses from 2 sidecar chunks"));
        assert!(applied.contains("reopened every selected pose"));
        assert_eq!(
            std::fs::read(source.join("dimensions/minecraft/overworld/entities/r.0.0.mca"))
                .expect("re-read source sidecar"),
            source_before,
            "native import never rewrites the Anvil entity sidecar"
        );
    }

    #[test]
    fn metadata_command_preflights_authorizes_and_reopens_typed_world_properties() {
        let _scratch = MetadataScratch::create();
        let root = Path::new(METADATA_SCRATCH);

        let source = root.join("anvil-source");
        let native_destination = root.join("native-destination");
        std::fs::create_dir(&source).expect("create fixture Anvil world directory");
        std::fs::write(source.join("level.dat"), LEVEL_DAT_FIXTURE)
            .expect("write checked-in level metadata fixture");
        let settings_path = source.join("data/minecraft/world_gen_settings.dat");
        std::fs::create_dir_all(settings_path.parent().expect("world-gen settings parent"))
            .expect("create world-generation settings directory");
        std::fs::write(&settings_path, WORLD_GEN_FIXTURE)
            .expect("write checked-in world-generation settings fixture");

        let import = [
            "import-metadata",
            "--source",
            source.to_str().expect("UTF-8 source path"),
            "--destination",
            native_destination.to_str().expect("UTF-8 destination path"),
            "--native-path",
            native_destination.to_str().expect("UTF-8 native path"),
        ];
        let preview = run(import).expect_err("metadata preview reports and refuses mutation");
        assert!(preview.contains("UnsupportedData"), "preflight reports discarded metadata");
        assert!(preview.contains("Refusing mutation without --apply."));
        assert!(
            !native_destination.exists(),
            "metadata preview cannot create the native destination"
        );

        let mut unacknowledged = import.to_vec();
        unacknowledged.push("--apply");
        let refusal = run(unacknowledged)
            .expect_err("lossy metadata conversion cannot apply without the review token");
        assert!(refusal.contains("Lossy conversion requires --acknowledge"));
        assert!(
            !native_destination.exists(),
            "unacknowledged metadata loss cannot create the native destination"
        );

        let mut approved = import.to_vec();
        approved.extend(["--apply", "--acknowledge", token(&preview)]);
        let applied = run(approved).expect("acknowledged metadata import commits typed properties");
        assert!(applied.contains("Converted 1 typed world-properties record"));
        assert_eq!(
            std::fs::read(source.join("level.dat")).expect("re-read source metadata"),
            LEVEL_DAT_FIXTURE,
            "conversion does not rewrite source metadata"
        );

        let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: native_destination,
        })
        .expect("reopen native destination after CLI conversion");
        let properties = reopened
            .load_world_properties()
            .expect("read typed world properties after filesystem reopen")
            .expect("CLI conversion committed world properties");
        assert_eq!(properties.game_data_version, 4_903);
        assert_eq!(properties.seed, -195_764_831);
        assert!(
            properties.day_time == 0,
            "unsupported total-age metadata is reported and not retained"
        );
    }

    #[test]
    fn player_export_command_publishes_the_native_snapshot() {
        let scratch = tempfile::tempdir().expect("create isolated player-export CLI scratch");
        let native = scratch.path().join("native");
        let destination = scratch.path().join("anvil");
        std::fs::create_dir(&destination).expect("create existing Anvil destination");
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: native.clone(),
        })
        .expect("open native player source");
        let uuid = uuid::Uuid::from_bytes([0x71; 16]);
        storage
            .write_dirty_player_data(NativePlayerData {
                locator: NativePlayerRecord {
                    uuid: *uuid.as_bytes(),
                    dimension: lodestone_storage_schema::BuiltinDimension::End,
                    x_fixed: -1_250,
                    y_fixed: 80_000,
                    z_fixed: 2_500,
                    yaw_millidegrees: 135_000,
                    pitch_millidegrees: -30_000,
                },
                game_mode: Some(lodestone_model::GameMode::Spectator),
            })
            .expect("seed typed native player");
        drop(storage);

        let output = run([
            "export-players",
            "--source",
            native.to_str().expect("UTF-8 native source"),
            "--destination",
            destination.to_str().expect("UTF-8 Anvil destination"),
            "--native-path",
            native.to_str().expect("UTF-8 native path"),
            "--apply",
        ])
        .expect("run native player export command");
        assert!(output.contains("Converted 1 typed native players"));
        let exported = lodestone_server::player_data::PlayerDataStore::new(&destination)
            .expect("open exported player directory")
            .read(uuid)
            .expect("decode exported player")
            .expect("exported player exists");
        assert_eq!(exported.dimension, "minecraft:the_end");
        assert_eq!(exported.pos, lodestone_model::Vec3::new(-1.25, 80.0, 2.5));
        assert_eq!(exported.game_mode, Some(lodestone_model::GameMode::Spectator));
    }
}
