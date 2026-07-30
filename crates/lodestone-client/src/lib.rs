//! Version-free Minecraft Java Edition client driver and public API.
//!
//! `lodestone-client` turns a [`VersionAdapter`] into a live, driven connection.
//! It contains **zero** version-specific knowledge: every protocol decision —
//! packet ids, state choreography, encoding — lives behind the adapter. The
//! driver only executes the [`Directive`]s an adapter emits and surfaces the
//! canonical [`ClientEvent`]s it produces.
//!
//! This is the crate users actually touch. It is library-first and treats
//! headless bots as a first-class use case: [`ClientBuilder::connect`] returns a
//! [`ClientHandle`] for submitting [`ClientAction`]s plus an [`EventStream`] to
//! observe the world, and the driver runs on its own task so nothing has to be
//! polled by hand.
//!
//! # How it drives a connection
//!
//! 1. [`VersionAdapter::begin_login`] is called and its directives executed in
//!    order.
//! 2. Each inbound packet is handed to [`VersionAdapter::handle_packet`] with the
//!    live [`ConnectionState`], and the returned directives are executed in order.
//! 3. Directive ordering is significant and preserved: a `SetState` only affects
//!    what follows it, and a `SetCompression` only affects packets written after
//!    it.
//!
//! Keep-alives are answered automatically by default (see [`KeepAlivePolicy`]),
//! and are always surfaced to the event stream regardless. Death is handled the
//! same way: the player auto-respawns by default (see [`RespawnPolicy`]) so
//! chunk streaming resumes, while the death event is still surfaced.
//!
//! # Example: a minimal bot
//!
//! ```no_run
//! use lodestone_client::{ClientBuilder, SessionOutcome};
//! use lodestone_model::{ClientEvent, LoginProfile, ServerAddress, VersionAdapter};
//! # use lodestone_model::{Directive, ClientAction, AdapterError, ConnectionState, WorldSink};
//! #
//! # // A real program passes an adapter from a protocol crate. This stub keeps
//! # // the example self-contained and version-free.
//! # #[derive(Debug)]
//! # struct MyAdapter;
//! # impl VersionAdapter for MyAdapter {
//! #     fn protocol_version(&self) -> i32 { 0 }
//! #     fn minecraft_versions(&self) -> &'static [&'static str] { &[] }
//! #     fn supports(&self, _: i32) -> bool { true }
//! #     fn begin_login(&self, _: &LoginProfile, _: &ServerAddress)
//! #         -> Result<Vec<Directive>, AdapterError> { Ok(vec![]) }
//! #     fn handle_packet(&self, _: &mut dyn WorldSink, _: ConnectionState, _: i32, _: &[u8])
//! #         -> Result<Vec<Directive>, AdapterError> { Ok(vec![]) }
//! #     fn encode_action(&self, _: ConnectionState, _: &ClientAction)
//! #         -> Result<Option<(i32, Vec<u8>)>, AdapterError> { Ok(None) }
//! # }
//! # fn adapter_for(_: i32) -> Box<dyn VersionAdapter> { Box::new(MyAdapter) }
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let server = ServerAddress { host: "localhost".into(), port: 25565 };
//! let profile = LoginProfile { username: "Bot".into(), uuid: uuid::Uuid::new_v4() };
//!
//! let (handle, mut events) =
//!     ClientBuilder::new(server, profile, adapter_for(776)).connect().await?;
//!
//! while let Some(event) = events.recv().await {
//!     if let ClientEvent::Login { .. } = event {
//!         handle.send_action(ClientAction::SendChat { text: "hello".into() })?;
//!     }
//! }
//!
//! match handle.join().await {
//!     SessionOutcome::ServerClosed => println!("server closed the connection"),
//!     SessionOutcome::ServerDisconnected { reason } => {
//!         println!("kicked: {}", reason.to_plain_string());
//!     }
//!     SessionOutcome::LocalClose => println!("we left"),
//!     SessionOutcome::Failed(error) => eprintln!("session failed: {error}"),
//!     _ => {}
//! }
//! # Ok(())
//! # }
//! ```

mod builder;
mod config;
mod driver;
mod error;
mod handle;
#[cfg(not(target_arch = "wasm32"))]
mod native_time;
mod spawn;
mod state;

pub use builder::ClientBuilder;
pub use config::{KeepAlivePolicy, PlayerLoadedPolicy, RespawnPolicy};
pub use error::{BotError, ClientClosed, ClientError, SessionOutcome, WaitError};
pub use handle::{ClientHandle, EventStream, WalkOutcome};
// Re-exported so a caller can build the `Session` `ClientBuilder::online_session`
// (issue #65) wants without a direct `lodestone-auth` dependency of their own —
// same reasoning as the `lodestone-game`/`lodestone-model` re-exports below.
#[cfg(not(target_arch = "wasm32"))]
pub use lodestone_auth::{AuthError, Session};
// Stage 3 of `docs/bevy-migration.md` deleted this crate's own `scoreboard`
// module — a second `Scoreboard`/`Objective`/`ScoreEntry`/`Team`/`BossBar` set
// folding the same `ClientEvent` stream as `lodestone-game`'s. The read-model
// now hands out `lodestone-game`'s aggregates, re-exported here so a bot author
// can name them without a second dependency.
pub use lodestone_game::bossbar::{BossBar, BossBarColor, BossBarOverlay, BossBarSet};
pub use lodestone_game::scoreboard::{
    CollisionRule as TeamCollisionRule, DisplaySlot as ScoreboardSlot, NumberFormat as ScoreFormat,
    Objective, RenderType as ScoreRenderType, ScoreEntry, Scoreboard, Team, TeamColor as TeamHue,
    Visibility as TeamVisibility,
};
pub use lodestone_game::tablist::{GameProfile, TabList};
pub use state::{EntityView, OpenMenuSnapshot, PlayerSnapshot};

// The world read-model hands out owned section and light snapshots; re-export the
// section and light types so consumers can name `Arc<ChunkSection>` and
// `SectionLight` without depending on `lodestone-world` directly. `LightData` is
// re-exported too so a mesher can branch on `SectionLight`'s `sky`/`block` fields
// (e.g. an above-the-world sky default) rather than only its accessors.
pub use lodestone_world::{ChunkSection, LightData, SectionLight};

// The dimension's vertical extent is a client-level view over that world
// snapshot (read from a loaded column's shape), so its type lives on the handle.
pub use handle::WorldDimensions;

// Re-export the model types a client user needs so they can build a session and
// use the bot API without depending on `lodestone-model` directly.
pub use lodestone_model::{
    BlockPos, BossAction, BossColor, BossOverlay, ChatAckInfo, ChatKind, ChunkPos, ClientAction,
    ClientEvent, CollisionRule, ConnectionState, DimensionId, Directive, DisplaySlot,
    EntityAttributeSnapshot, GameMode, Hand, LoginProfile, NumberFormat, ObjectiveMode,
    ObjectiveRenderType, PlayerListEntry, Reported, ResourceKey, Rotation, ServerAddress,
    TeamAction, TeamColor, TeamParameters, Text, Vec3, VersionAdapter, Visibility,
};
