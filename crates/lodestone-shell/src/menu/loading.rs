//! The connect/load phase names and the terrain progress count behind the
//! loading screen (issue #449).
//!
//! # What it is
//!
//! Two small pieces of shared vocabulary, deliberately kept out of both the
//! draw and the net layer:
//!
//! * [`ConnectPhase`] — which step of establishing a session we are on, and the
//!   **real vanilla string** for it. Not a cosmetic label: the whole point of
//!   the issue is that "still loading" and "broken" were indistinguishable, and
//!   three separate defects in this repo had "the game is frozen" as their only
//!   symptom (a dead player held on the death screen sending no chunks,
//!   `PERFORM_RESPAWN` decoded and discarded, LAN hosting with no tick loop).
//!   A screen that names its step turns those into "stuck at *this* step".
//! * [`TerrainProgress`] — the loaded-column count against the count the server
//!   is going to send, which is what the progress bar is derived from.
//!
//! # The rule this module exists to enforce
//!
//! **Every string here is a real key from the 26.2 `en_us.json`, and every
//! number is measured, never synthesised.** A progress screen showing fake
//! progress is worse than no screen at all, because it converts an honest
//! freeze into a false reassurance. So:
//!
//! * The labels are transcribed from
//!   `.cache/mc/26.2/client-src/assets/minecraft/lang/en_us.json`, and each
//!   variant records its key. Note that 26.2 has **no** `menu.loadingLevel` or
//!   `menu.generatingTerrain` — those were removed — which is why the
//!   singleplayer world-open shares [`ConnectPhase::Connecting`] rather than
//!   getting an invented string of its own. That is also what vanilla does:
//!   its integrated server is reached over a real connection, so singleplayer
//!   and multiplayer go through the same connect screens.
//! * [`TerrainProgress`] carries the raw numerator and denominator rather than a
//!   pre-computed percentage, so a caller cannot round a partial load up to
//!   "done", and [`TerrainProgress::fraction`] is clamped **below** 1.0 for
//!   exactly that reason — the screen closes when the real predicate
//!   (`Sim::terrain_loading`) says the player's own column has landed, never
//!   because a bar filled.
//!
//! # How to change it
//!
//! To add a phase you need a **real boundary in `net.rs`'s connect task** to
//! emit it from — a new variant with no emit site is the island pattern
//! `CLAUDE.md` §1 describes, and it will render as a phase the game never
//! reaches. The emit sites are `NetUpdate::ConnectPhase` sends in
//! `crate::net::run_session`; the route is `sim/net_apply.rs` →
//! `Sim::connect_phase` → `app::WindowApp::drive_ui_from_session` →
//! `UiState::set_connect_phase` → `menu/render/dispatch.rs`.
//!
//! **`connect.authorizing`, `connect.encrypting` and `connect.negotiating` are
//! deliberately absent.** Vanilla sets them from inside
//! `ClientHandshakePacketListenerImpl`, i.e. from the handshake state machine
//! itself. Ours lives in `lodestone-client`, behind one infallible
//! `ClientBuilder::connect().await`, so the shell cannot observe those three
//! boundaries at all. Adding them would mean the label changing on a timer
//! rather than on an event — the fake-progress failure this module exists to
//! avoid. They become available if and when the client driver reports the
//! handshake stage; until then their absence is honest.

/// Which step of establishing a session the loading screen is naming.
///
/// See the module doc: each variant is one real boundary the shell can actually
/// observe, and its label is the real 26.2 string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectPhase {
    /// `connect.connecting`. The session task has started: for multiplayer the
    /// socket is being dialled and the handshake/login run; for singleplayer the
    /// integrated server is being opened (world seed resolved, region source and
    /// generator built) and then reached over the in-memory transport. One phase
    /// covers both because vanilla's integrated server is likewise reached over
    /// a real connection.
    #[default]
    Connecting,
    /// `connect.joining`. The client handle exists — the handshake and login
    /// completed — and we are waiting for the play state to be usable.
    Joining,
    /// `multiplayer.downloadingTerrain`. Logged in; the server is streaming the
    /// initial view. This is the phase the progress bar belongs to, and on a
    /// brand-new singleplayer world it is also when generation happens, because
    /// columns are generated lazily as they are streamed.
    LoadingTerrain,
}

impl ConnectPhase {
    /// The vanilla translation key, recorded so a future i18n table has
    /// something to look up and so the label below is auditable against the jar.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Connecting => "connect.connecting",
            Self::Joining => "connect.joining",
            Self::LoadingTerrain => "multiplayer.downloadingTerrain",
        }
    }

    /// The `en_us` string for [`Self::key`], transcribed from the 26.2 jar.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting to the server...",
            Self::Joining => "Joining world...",
            Self::LoadingTerrain => "Loading terrain...",
        }
    }
}

/// How much of the initial view has landed: `loaded` columns out of `expected`.
///
/// `expected` is the count the server is actually going to send — the view
/// square `(2 * view_radius + 1)^2`, the same square `join_view_rings`
/// partitions — not a guess and not a running maximum. `loaded` is the client's
/// own loaded-column count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerrainProgress {
    /// Columns the client has applied.
    pub loaded: usize,
    /// Columns the initial view contains.
    pub expected: usize,
}

impl TerrainProgress {
    /// The view square for a `view_radius`, in columns.
    #[must_use]
    pub const fn expected_for_radius(view_radius: u32) -> usize {
        let side = (view_radius as usize) * 2 + 1;
        side * side
    }

    /// The bar fill, in `0.0..=MAX_FRACTION`.
    ///
    /// **Clamped below 1.0 deliberately.** The loading screen is dismissed by
    /// `Sim::terrain_loading` — a real test on whether the player's own column
    /// has arrived — never by this number reaching the end. A bar that could
    /// read as full while the screen is still up would be the false
    /// reassurance this whole feature exists to prevent; leaving the last
    /// sliver unfilled means "not done" stays visible even when the count
    /// happens to agree.
    #[must_use]
    pub fn fraction(self) -> f32 {
        if self.expected == 0 {
            return 0.0;
        }
        let raw = self.loaded as f32 / self.expected as f32;
        raw.clamp(0.0, MAX_FRACTION)
    }

    /// The count line drawn under the bar — the honest raw numbers, so a stall
    /// is legible as "stuck at 37/441" rather than as a bar that stopped.
    #[must_use]
    pub fn detail(self) -> String {
        format!("{} / {} chunks", self.loaded, self.expected)
    }
}

/// The most the progress bar will ever report. See [`TerrainProgress::fraction`].
pub const MAX_FRACTION: f32 = 0.99;

#[cfg(test)]
mod tests {
    use super::{ConnectPhase, MAX_FRACTION, TerrainProgress};

    /// Every phase must have both a key and a non-empty label, and no two
    /// phases may share either — a duplicated label is a phase the screen
    /// cannot distinguish, which defeats the point.
    #[test]
    fn every_phase_has_a_distinct_key_and_label() {
        let all = [
            ConnectPhase::Connecting,
            ConnectPhase::Joining,
            ConnectPhase::LoadingTerrain,
        ];
        for phase in all {
            assert!(phase.key().contains('.'), "{phase:?} key must be a real key");
            assert!(!phase.label().is_empty());
        }
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.key(), b.key());
                assert_ne!(a.label(), b.label());
            }
        }
    }

    /// The denominator is the view square, and the bar can never claim
    /// completion — the two properties the module doc calls load-bearing.
    #[test]
    fn progress_uses_the_view_square_and_never_claims_completion() {
        // A render distance of 10 gives view_radius 11 -> 23x23 = 529.
        assert_eq!(TerrainProgress::expected_for_radius(11), 529);
        assert_eq!(TerrainProgress::expected_for_radius(0), 1);

        let empty = TerrainProgress { loaded: 0, expected: 529 };
        assert_eq!(empty.fraction(), 0.0);

        let full = TerrainProgress { loaded: 529, expected: 529 };
        assert_eq!(full.fraction(), MAX_FRACTION);

        // Over-count (the server sent more than the square, e.g. the player
        // moved mid-load) still cannot read as done.
        let over = TerrainProgress { loaded: 900, expected: 529 };
        assert_eq!(over.fraction(), MAX_FRACTION);

        // A zero denominator is a missing view radius, not a divide by zero.
        let unknown = TerrainProgress { loaded: 5, expected: 0 };
        assert_eq!(unknown.fraction(), 0.0);

        assert_eq!(
            TerrainProgress { loaded: 37, expected: 441 }.detail(),
            "37 / 441 chunks"
        );
    }
}
