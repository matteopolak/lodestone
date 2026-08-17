//! The command seam: how a serverbound `chat_command` frame reaches a
//! dispatcher that lives on the other side of this crate's dependency
//! boundary.
//!
//! # What it is
//!
//! A one-method trait ([`CommandSink`]) plus a cheap, cloneable handle
//! ([`CommandDispatch`]) the host installs. `crate::server`'s
//! [`ServerBound::ChatCommand`](crate::ServerBound::ChatCommand) arm calls it
//! and turns whatever comes back into system-chat directives on the calling
//! connection.
//!
//! # Why a seam at all, rather than just calling the dispatcher
//!
//! Plugin commands are registered into `lodestone_ecs::commands::CommandRegistry`,
//! and dispatched by `lodestone_ecs::commands::dispatch(&mut World, ..)`. That
//! registry lives in `lodestone-ecs` because that is where the plugin API lives
//! — a registry in *this* crate would be unreachable by every plugin that can
//! exist, which is why an earlier proposal along those lines was unbuildable
//! as written.
//!
//! But **this crate deliberately depends on neither `lodestone-ecs` nor the
//! client vocabulary it carries**, and that is a measured decision, not an
//! accident: see this crate's `Cargo.toml`, which records that linking
//! `lodestone-ecs` would drag `LocalPlayer`/`FrameClock`/`SessionMenus` plus
//! `lodestone-physics`/`-game`/`-world` into this graph *and into the browser
//! bundle*, which links `lodestone-server` and links neither today.
//!
//! There were three ways out, considered and rejected here so the next reader
//! does not have to re-derive it — this module is the one taken:
//!
//! | option | why not |
//! |---|---|
//! | 1. add the `lodestone-ecs` dependency | contradicts the boundary above, and the cost is already measured in `Cargo.toml` — the browser bundle is the concrete loser |
//! | 3. move dispatch server-side, mirror the registry | reintroduces exactly the flat-vs-arena duplication an earlier decision declined to create, and leaves plugins registering into a registry that is not the one dispatch reads |
//! | **2. a callback/queue seam the host installs** | **taken.** Matches the intent doctrine the rest of this seam already uses — pre-computed answers handed across, never a query back — and inverts the dependency so the *host*, which already links both crates, owns the glue |
//!
//! # The vocabulary is deliberately version-free and ECS-free
//!
//! [`CommandCaller`] carries a `Uuid` and a `String`; [`CommandResponse`]
//! carries `String`s. Nothing here names a protocol number, a packet id, a
//! `World`, or a `Resource`. That is what lets the host implement
//! [`CommandSink`] over `lodestone-ecs` without this crate ever seeing it.
//!
//! # Does this generalise to the other 42 stranded serverbound variants?
//!
//! **Deliberately not, and the reason is worth stating.** `cargo xtask
//! connectedness` reports 43 `v770` serverbound variants that decode and land
//! in `server.rs`'s `ServerBound::Ignored => {}`. Almost all of them —
//! `SWING`, `INTERACT`, `MOVE_PLAYER_ROT`, `PLAYER_ABILITIES` — are stranded
//! because the *gameplay* is unimplemented, and their consumer belongs in this
//! crate. Routing those through a host callback would be strictly worse: it
//! would move server behaviour out of the server.
//!
//! The axis this seam generalises along is narrower and real: **packets whose
//! consumer structurally cannot live in this crate because it lives in the
//! plugin API.** Today that is `chat_command`. The next one is `custom_payload`
//! (`lodestone_ecs::plugin_message`), which is in the same 43 and has the same
//! shape. [`CommandSink`] is therefore a trait with a named method rather than
//! a bare `Fn(&str)`, so a second method can be added for it without changing
//! any signature this crate exposes.
//!
//! # How to change it
//!
//! * Adding a host-dispatched packet kind: add a method to [`CommandSink`]
//!   **with a default body** that refuses, so an existing host impl keeps
//!   compiling and an un-updated host fails closed rather than open.
//! * Do not add a `&mut World`-shaped parameter here in any disguise. The
//!   moment this trait names an ECS type the boundary above is gone.
//!
//! # The security property, stated because its failure mode is silent
//!
//! **No sink installed must mean "nothing runs", never "everything runs".**
//! [`CommandDispatch::default`] holds no sink and
//! [`CommandDispatch::run`] answers [`CommandResponse::refused`] without
//! consulting anything. This mirrors
//! `plugin_command_registry`'s
//! `dispatch_refuses_rather_than_ungates_when_permissions_are_missing`
//! one layer out:
//! a missing resource, and now a missing *sink*, both refuse.
//!
//! Note what this layer can and cannot enforce. It cannot check a permission —
//! it has no `Permissions` resource and, by the boundary above, never will.
//! What it enforces instead is that the sink is handed the **connection's own
//! authenticated identity** ([`CommandCaller`], built from the uuid
//! `login_success` echoed and the username the login carried), never anything
//! the command text can influence. A sink therefore cannot be tricked into
//! resolving permissions for a caller other than the one that sent the frame.

use std::fmt;
use std::sync::Arc;

use uuid::Uuid;

/// Who sent a command.
///
/// Built by `crate::server` from the connection's own login, **not** from
/// anything in the command text. See this module's doc comment for why that
/// matters: this is the only identity a [`CommandSink`] gets, so permission
/// resolution on the far side cannot be aimed at a different player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandCaller {
    /// The profile id this connection logged in with — the same uuid
    /// `ServerProtocol::login_success` echoed back, so the far side can look
    /// the player up in its own world.
    pub uuid: Uuid,
    /// The username this connection logged in with.
    pub username: String,
}

impl CommandCaller {
    /// A caller with the given identity.
    #[must_use]
    pub fn new(uuid: Uuid, username: impl Into<String>) -> Self {
        Self {
            uuid,
            username: username.into(),
        }
    }
}

/// What the host did with a command.
///
/// Deliberately not a `Result`: a refusal is an ordinary, expected outcome
/// (unknown command, bad argument, missing permission) that the player must be
/// *told about*, not an error the connection layer should react to. Both
/// variants produce system chat and neither ends the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandResponse {
    /// The command ran. `feedback` is whatever should be shown to the caller,
    /// one system-chat line each; empty is legal and common (vanilla commands
    /// frequently succeed silently when `sendCommandFeedback` is off).
    Ran {
        /// Lines to send back to the caller, in order.
        feedback: Vec<String>,
    },
    /// The command did not run, and this is why.
    ///
    /// Covers unknown command, parse failure, **and permission denial** — the
    /// wire layer deliberately cannot tell those apart, because distinguishing
    /// them here would mean this crate knowing what a permission is.
    Refused {
        /// The single line explaining the refusal.
        message: String,
    },
}

impl CommandResponse {
    /// A refusal carrying `message`.
    #[must_use]
    pub fn refused(message: impl Into<String>) -> Self {
        Self::Refused {
            message: message.into(),
        }
    }

    /// A silent success.
    #[must_use]
    pub fn ran() -> Self {
        Self::Ran {
            feedback: Vec::new(),
        }
    }

    /// Whether the command ran.
    #[must_use]
    pub fn is_ran(&self) -> bool {
        matches!(self, Self::Ran { .. })
    }

    /// The lines this response wants shown to the caller, in order.
    #[must_use]
    pub fn lines(&self) -> &[String] {
        match self {
            Self::Ran { feedback } => feedback,
            Self::Refused { message } => std::slice::from_ref(message),
        }
    }
}

/// What vanilla's `CommandDispatcher` answers an unrecognised root with, and
/// what this crate answers with when **no sink is installed at all**.
///
/// Kept as one constant so the "no sink" path and a host that wants to match
/// it cannot drift apart, and so a test can assert the exact string rather
/// than that *some* refusal happened.
pub const UNKNOWN_COMMAND: &str = "Unknown or incomplete command, see below for error";

/// The host-installed command dispatcher.
///
/// Implemented on the far side of this crate's dependency boundary — by
/// whichever crate links both `lodestone-server` and the ECS the plugin
/// registry lives in — and installed with
/// [`CommandDispatch::installed`].
///
/// `&self`, not `&mut self`: a connection task calls this and several
/// connections may exist, so the implementor owns its own synchronisation.
/// The ECS-side implementor needs `&mut World`, which means an interior
/// `Mutex`; that is the implementor's problem by design, because making it
/// this crate's problem would mean this crate knowing what a `World` is.
pub trait CommandSink: Send + Sync {
    /// Run `command` (already stripped of its leading `/` by the wire format —
    /// vanilla's `ServerboundChatCommandPacket` carries it without one) on
    /// behalf of `caller`.
    ///
    /// Must not panic: this runs on a connection task, and a panic here takes
    /// the player's connection with it.
    fn run(&self, caller: &CommandCaller, command: &str) -> CommandResponse;
}

/// A cheap, cloneable handle to the host's [`CommandSink`], or to no sink at
/// all.
///
/// Shaped like [`crate::BlockTickFeed`] and `ExplosionFeed`: a `Default` that
/// is inert, so every existing `serve_connection*` entry point can pass one
/// without changing behaviour, and one new entry point takes a live one. That
/// is what keeps this change off every call site in
/// `crates/protocol/v770/tests/*` and out of `integrated.rs`.
#[derive(Clone, Default)]
pub struct CommandDispatch {
    sink: Option<Arc<dyn CommandSink>>,
}

impl fmt::Debug for CommandDispatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandDispatch")
            .field("installed", &self.sink.is_some())
            .finish()
    }
}

impl CommandDispatch {
    /// A dispatch with no sink: every command is refused with
    /// [`UNKNOWN_COMMAND`].
    ///
    /// This is [`Default`], and that is the load-bearing part — see the module
    /// doc's security note. The absence of a dispatcher must never read as
    /// blanket permission.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// A dispatch backed by `sink`.
    #[must_use]
    pub fn installed(sink: Arc<dyn CommandSink>) -> Self {
        Self { sink: Some(sink) }
    }

    /// Whether a sink is installed.
    ///
    /// Exists for the negative control: a gate that asserts a command did
    /// *not* run needs to be able to say the detector was armed.
    #[must_use]
    pub fn is_installed(&self) -> bool {
        self.sink.is_some()
    }

    /// Run `command` for `caller`, or refuse if no sink is installed.
    #[must_use]
    pub fn run(&self, caller: &CommandCaller, command: &str) -> CommandResponse {
        match &self.sink {
            Some(sink) => sink.run(caller, command),
            None => CommandResponse::refused(UNKNOWN_COMMAND),
        }
    }
}

/// Everything the Play loop needs to service one connection's commands: the
/// built-in tree, the host's dispatch, this connection's authenticated identity,
/// and its permission level.
///
/// Bundled into one struct rather than passed as four parameters because
/// `dispatch_play_packet` already takes 24 arguments; this adds one.
///
/// # `builtins` comes first, and `dispatch` is the fallback
///
/// The built-in tree ([`crate::ServerCommands`]) is consulted before the host
/// sink, and answers `None` only when no built-in root matched at all — see
/// `crate::commands`' precedence table. Before that wiring existed, the built-in
/// tree had no caller anywhere and every command went straight to a sink that
/// every real constructor leaves empty, which meant `/gamerule` did nothing.
#[derive(Clone, Debug)]
pub(crate) struct CommandSession {
    /// The server's own commands. Cheap to clone (one `Arc`).
    pub(crate) builtins: crate::commands::ServerCommands,
    pub(crate) dispatch: CommandDispatch,
    pub(crate) caller: CommandCaller,
    /// This caller's permission level, 0–4, resolved once at the Play handoff
    /// from [`crate::AccessLists::permission_level`].
    ///
    /// Resolved **once**, not per command, and from the connection's *own*
    /// authenticated uuid — the same property [`CommandCaller`]'s doc comment
    /// describes, for the same reason: nothing in the command text may influence
    /// which player's permissions are consulted.
    pub(crate) permission_level: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller() -> CommandCaller {
        CommandCaller::new(Uuid::from_u128(7), "tester")
    }

    /// The security property this module exists to hold, at its own layer.
    #[test]
    fn a_dispatch_with_no_sink_refuses_rather_than_running() {
        let dispatch = CommandDispatch::none();
        assert!(!dispatch.is_installed());
        assert_eq!(
            dispatch.run(&caller(), "warp spawn"),
            CommandResponse::refused(UNKNOWN_COMMAND)
        );

        // The control: install a sink that would have run, and the identical
        // call now runs — so the refusal above was about the missing sink and
        // not about the input being rejected for some other reason.
        struct Yes;
        impl CommandSink for Yes {
            fn run(&self, _: &CommandCaller, _: &str) -> CommandResponse {
                CommandResponse::ran()
            }
        }
        let dispatch = CommandDispatch::installed(Arc::new(Yes));
        assert!(dispatch.run(&caller(), "warp spawn").is_ran());
    }

    #[test]
    fn the_sink_receives_the_command_without_a_leading_slash_and_the_callers_identity() {
        use std::sync::Mutex;

        #[derive(Default)]
        struct Recorder(Mutex<Vec<(Uuid, String, String)>>);
        impl CommandSink for Recorder {
            fn run(&self, caller: &CommandCaller, command: &str) -> CommandResponse {
                self.0.lock().unwrap().push((
                    caller.uuid,
                    caller.username.clone(),
                    command.to_owned(),
                ));
                CommandResponse::ran()
            }
        }

        let recorder = Arc::new(Recorder::default());
        let dispatch = CommandDispatch::installed(recorder.clone());
        let _ = dispatch.run(&caller(), "warp spawn");

        let seen = recorder.0.lock().unwrap();
        assert_eq!(
            seen.as_slice(),
            &[(Uuid::from_u128(7), "tester".to_owned(), "warp spawn".to_owned())]
        );
    }

    #[test]
    fn a_refusal_carries_exactly_one_line_and_a_silent_success_carries_none() {
        assert_eq!(CommandResponse::refused("no").lines().len(), 1);
        assert_eq!(CommandResponse::ran().lines().len(), 0);
        assert_eq!(
            CommandResponse::Ran {
                feedback: vec!["a".to_owned(), "b".to_owned()],
            }
            .lines()
            .len(),
            2
        );
    }
}
