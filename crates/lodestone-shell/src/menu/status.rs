//! Per-server status pings for the multiplayer list, and the cache that holds
//! their results.
//!
//! ## What it is
//!
//! The server list shows a MOTD, a player count and a favicon per row. Getting
//! them means a status ping per server, which is network I/O and must not run on
//! the frame thread. This module owns that: a [`StatusCache`] keyed by address,
//! one background thread per in-flight probe, and a [`StatusSlot`] per row that
//! the renderer reads without blocking.
//!
//! ## What the probe actually is
//!
//! [`net_probe`] calls `lodestone_net::server_status`, which resolves SRV per the
//! vanilla rules, performs the modern handshake/status exchange, and decodes
//! MOTD / player counts / favicon. It is the default probe
//! ([`StatusCache::new`]) rather than something a caller must remember to
//! install: a default that quietly did nothing is precisely the shape of defect
//! this repo keeps hitting.
//!
//! **`lodestone_net::ping` and `lodestone_net::resolve` had no consumer anywhere
//! in the workspace before this.** They were complete, unit-tested, exported —
//! and dead. The only code in the tree that pinged a server,
//! `lodestone-game/tests/live_server.rs`, hand-rolled the entire status
//! handshake over a raw `TcpStream` rather than calling them. This module is the
//! first caller; the manifest edge in `lodestone-shell/Cargo.toml` is the line
//! that closes it.
//!
//! The probe stays behind the [`Probe`] type alias because it is *blocking* work
//! that must not run on the frame thread, and because tests need a deterministic
//! stand-in. [`unavailable_probe`] is kept for that, and for any build where
//! networking should be refused outright.
//!
//! ## How to change it
//!
//! [`StatusCache::refresh`] is idempotent per address and will not re-probe a
//! row that is already in flight; call it whenever the list is shown or an entry
//! is edited. [`StatusCache::pump`] must be called each frame to move finished
//! probes from the channel into slots — nothing else drains it.
//!
//! Each probe builds its own single-threaded tokio runtime on its own thread.
//! That is cheap relative to a DNS lookup plus a TCP round trip, and it avoids
//! the shell owning a second runtime just for the menu; if the server list ever
//! grows to hundreds of rows, replace the thread-per-probe with a small pool
//! before optimising anything else here.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use super::servers::ServerEntry;

/// A decoded status, in the form the list renders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerStatus {
    /// MOTD, already flattened to plain text. Used for layout and for the
    /// one-line [`StatusSlot::summary`]; the *drawn* MOTD is
    /// [`Self::motd_spans`].
    pub motd: String,
    /// MOTD as styled runs, carrying the server's colours.
    ///
    /// Carried alongside the plain string rather than replacing it because the
    /// two are wanted in different places — wrapping and truncation reason about
    /// characters, the draw reasons about colour — and because deriving one from
    /// the other at each use site is how the flat-colour bug survived: every
    /// layer had a `String` and none of them had lost anything *locally*.
    pub motd_spans: Vec<lodestone_model::text::TextSpan>,
    /// Player count, pre-rendered as `online/max`.
    pub players: String,
    /// Players online, when the server reports it — the *numeric* truth behind
    /// the [`Self::players`] line's first term.
    ///
    /// Carried alongside the rendered string rather than folded into it because
    /// the tooltip needs the count, not the line: vanilla appends
    /// `multiplayer.status.and_more` ("... and N more ...") when the sample is
    /// short of it (`ServerStatusPinger.java:90-110`), and that arithmetic is
    /// [`player_sample_lines`]'s job, not the probe's.
    pub online: Option<u32>,
    /// Online players' names, from the status `sample`, in server order.
    ///
    /// This is what a "who's online" tooltip reads (vanilla
    /// `ServerSelectionList.java:410,430`). Plain names rather than the full
    /// `(id, name)` pairs the net layer decodes, because the row only displays
    /// names — the anonymous-profile shaping vanilla applies per id
    /// (`ServerStatusPinger.java:99-104`) needs the profile the shell drops,
    /// and the "and N more" shaping needs the numeric [`Self::online`] count
    /// carried alongside; both belong to the tooltip ([`player_sample_lines`]),
    /// not to this model.
    pub sample: Vec<String>,
    /// Server version name, e.g. `"26.2"`.
    pub version: String,
    /// The protocol number the server reported, when it reported one.
    ///
    /// This is what decides [`ServerState::Incompatible`], so it is not
    /// cosmetic: vanilla compares `serverData.protocol` with
    /// `SharedConstants.getCurrentVersion().protocolVersion()` and paints
    /// `server_list/incompatible` plus the version string in red on any
    /// mismatch (`ServerSelectionList.java:284-288,344-346`). A server that
    /// omits `version.protocol` therefore reads as incompatible, in vanilla
    /// (where the field defaults to `0`) and here (where it is `None`) alike.
    pub protocol: Option<i32>,
    /// Favicon PNG bytes, when the server sent a usable one.
    pub favicon_png: Option<Vec<u8>>,
    /// Ping round-trip, in milliseconds.
    pub latency_ms: Option<u64>,
}

/// The lines a "who's online" tooltip draws for a status's sample — vanilla's
/// `data.playerList` (`ServerStatusPinger.java:90-110`): the sample's names in
/// order, then `multiplayer.status.and_more` ("... and %s more ...") when the
/// sample is short of the reported online count.
///
/// An empty sample returns an empty list, matching vanilla's
/// `else { data.playerList = List.of() }` (`:109`) — and an empty
/// `playerList` draws no tooltip at all (`setTooltipForNextFrameInternal`'s
/// `if (!lines.isEmpty())` guard), so "no tooltip" and "nothing to say" are
/// the same value here, by design.
///
/// Vanilla's anonymous-player shaping
/// (`MinecraftServer.ANONYMOUS_PLAYER_PROFILE` →
/// `multiplayer.status.anonymous_player`, `:99-104`) needs the profile id this
/// display model deliberately drops, so it is not reproduced; the shell shows
/// the name the server sent.
#[must_use]
pub fn player_sample_lines(sample: &[String], online: Option<u32>) -> Vec<String> {
    if sample.is_empty() {
        return Vec::new();
    }
    let mut lines = sample.to_vec();
    if let Some(online) = online
        && sample.len() < online as usize
    {
        lines.push(format!("... and {} more ...", online as usize - sample.len()));
    }
    lines
}

/// The state of one row's status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusSlot {
    /// Never probed.
    Idle,
    /// A probe is in flight.
    Pending,
    /// The server answered.
    Ok(Box<ServerStatus>),
    /// The probe failed; the payload is why, for display.
    Failed(String),
}

impl StatusSlot {
    /// A one-line summary for a list row.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            StatusSlot::Idle => "—".to_string(),
            StatusSlot::Pending => "pinging…".to_string(),
            StatusSlot::Ok(s) => {
                let first = s.motd.split('\n').next().unwrap_or("");
                if first.is_empty() {
                    s.players.clone()
                } else {
                    format!("{first}  ({})", s.players)
                }
            }
            StatusSlot::Failed(why) => format!("! {why}"),
        }
    }

    /// Whether a probe is currently running for this slot.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        matches!(self, StatusSlot::Pending)
    }

    /// Vanilla's `ServerData.State` for this slot, given the protocol *we*
    /// speak.
    ///
    /// The mapping is one-to-one except at the top: vanilla has a distinct
    /// `INITIAL` state that exists for exactly one frame (`extractContent`
    /// flips it to `PINGING` the first time it draws a row,
    /// `ServerSelectionList.java:269-271`), and [`StatusSlot::Idle`] is the
    /// same "no probe has been started" fact.
    #[must_use]
    pub fn state(&self, our_protocol: i32) -> ServerState {
        match self {
            StatusSlot::Idle => ServerState::Initial,
            StatusSlot::Pending => ServerState::Pinging,
            StatusSlot::Failed(_) => ServerState::Unreachable,
            StatusSlot::Ok(s) => {
                if s.protocol == Some(our_protocol) {
                    ServerState::Successful
                } else {
                    ServerState::Incompatible
                }
            }
        }
    }
}

/// Vanilla's `ServerData.State` — which of the five presentations a row is in.
///
/// Kept as its own enum rather than folded into [`StatusSlot`] because the
/// *transport* result (answered / failed / in flight) and the *presentation*
/// state are different questions: one answered probe splits into
/// [`Self::Successful`] and [`Self::Incompatible`] depending on a protocol
/// number, and that comparison needs a value [`StatusSlot`] does not carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    /// Never probed. Vanilla's `INITIAL`.
    Initial,
    /// A probe is in flight.
    Pinging,
    /// Answered, and speaking our protocol.
    Successful,
    /// Answered, but speaking a different protocol.
    Incompatible,
    /// Did not answer.
    Unreachable,
}

/// `server_list/ping_1` … `ping_5`, weakest signal first — the order the
/// latency buckets in [`ping_sprite`] index into.
pub const PING_SPRITES: [&str; 5] = [
    "server_list/ping_1",
    "server_list/ping_2",
    "server_list/ping_3",
    "server_list/ping_4",
    "server_list/ping_5",
];

/// `server_list/pinging_1` … `pinging_5`, the animation frames
/// [`pinging_sprite`] cycles through.
pub const PINGING_SPRITES: [&str; 5] = [
    "server_list/pinging_1",
    "server_list/pinging_2",
    "server_list/pinging_3",
    "server_list/pinging_4",
    "server_list/pinging_5",
];

/// What the MOTD column says while a probe is in flight.
///
/// `multiplayer.status.pinging`'s `en_us` string, and it goes in the **MOTD**
/// slot rather than the status one because that is what vanilla does:
/// `ServerStatusPinger.pingServer` assigns `data.motd = translatable(
/// "multiplayer.status.pinging")` and blanks `data.status`
/// (`ServerStatusPinger.java:65`).
pub const PINGING_MOTD: &str = "Pinging...";

/// `ServerSelectionList.INCOMPATIBLE_SPRITE`.
pub const INCOMPATIBLE_SPRITE: &str = "server_list/incompatible";
/// `ServerSelectionList.UNREACHABLE_SPRITE`.
pub const UNREACHABLE_SPRITE: &str = "server_list/unreachable";

/// The signal-strength sprite for a round-trip time, from
/// `ServerSelectionList.refreshStatus`'s `SUCCESSFUL` arm (`:417-427`).
///
/// The buckets are `< 150`, `< 300`, `< 600`, `< 1000`, else — and note they
/// run *downward*: a fast server gets `ping_5` (five bars) and a slow one
/// `ping_1`. `None` becomes `0`, because vanilla's `serverData.ping` is a
/// primitive `long` that is simply still zero when a status arrived without a
/// measured round trip.
#[must_use]
pub fn ping_sprite(ping_ms: Option<u64>) -> &'static str {
    let ping = ping_ms.unwrap_or(0);
    if ping < 150 {
        PING_SPRITES[4]
    } else if ping < 300 {
        PING_SPRITES[3]
    } else if ping < 600 {
        PING_SPRITES[2]
    } else if ping < 1000 {
        PING_SPRITES[1]
    } else {
        PING_SPRITES[0]
    }
}

/// The animated pinging sprite for row `index` at `millis`, from
/// `ServerSelectionList.extractContent` (`:315-327`).
///
/// `(millis / 100 + index * 2) & 7` gives 0..=7 and the `if idx > 4 { idx = 8 -
/// idx }` fold turns that into a **ping-pong** over 0..=4 rather than a sawtooth
/// — 5 becomes 3, 6 becomes 2, 7 becomes 1 — so the bars sweep up and back down
/// instead of snapping. The `index * 2` term is what makes two adjacent rows
/// animate out of phase.
#[must_use]
pub fn pinging_sprite(millis: u64, index: usize) -> &'static str {
    let phase = (millis / 100).wrapping_add((index as u64).wrapping_mul(2)) & 7;
    let mut idx = usize::try_from(phase).unwrap_or(0);
    if idx > 4 {
        idx = 8 - idx;
    }
    PINGING_SPRITES[idx]
}

/// The status sprite a row draws, for every state.
///
/// `Initial` is `ping_1` rather than a pinging frame because that is what
/// `refreshStatus` sets for both `INITIAL` and `PINGING` (`:402-406`); the
/// animation in `extractContent` then *overrides* it, but only once the state
/// has actually moved to `PINGING`.
#[must_use]
pub fn status_sprite(
    state: ServerState,
    ping_ms: Option<u64>,
    millis: u64,
    index: usize,
) -> &'static str {
    match state {
        ServerState::Initial => PING_SPRITES[0],
        ServerState::Pinging => pinging_sprite(millis, index),
        ServerState::Successful => ping_sprite(ping_ms),
        ServerState::Incompatible => INCOMPATIBLE_SPRITE,
        ServerState::Unreachable => UNREACHABLE_SPRITE,
    }
}

/// A blocking status probe. Runs on a background thread, never the frame thread.
pub type Probe = Arc<dyn Fn(&ServerEntry) -> Result<ServerStatus, String> + Send + Sync>;

/// Protocol number advertised in the status handshake. Vanilla ignores it in the
/// status state, but a proxy may use it to pick a backend, so it should match
/// what the client would actually connect with.
pub const STATUS_PROTOCOL: i32 = 776;

/// The real probe: `lodestone_net::server_status` on a private runtime.
///
/// `entry.port` is passed as `Option`, **not** `effective_port()`. That is
/// load-bearing: `lodestone-net` only performs the `_minecraft._tcp` SRV lookup
/// when no port was pinned, so collapsing `None` into `Some(25565)` here would
/// make every SRV-only server in the list unreachable while looking like a
/// connection failure.
/// # Browser (`wasm32`)
///
/// Returns a probe that always fails with a reason naming the actual obstacle,
/// rather than one that silently reports nothing. Two things are missing there and
/// **both** are structural: `lodestone_net::server_status` is `cfg`-gated off wasm
/// because it opens a raw `TcpStream`, which a page cannot do at all; and the
/// runtime this function builds to `block_on` is a blocking wait on the one thread
/// the browser uses to paint. A browser server list needs the WebSocket relay
/// (`ws-web`) and an `async` probe driven off `spawn_local`, which is a different
/// function, not a shim over this one. `unavailable_probe`'s docs below say why the
/// reason has to be explicit; the same argument applies here.
#[must_use]
pub fn net_probe(protocol: i32) -> Probe {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = protocol;
        return Arc::new(|entry: &ServerEntry| {
            Err(format!(
                "cannot ping {} from a browser: a page has no raw TCP socket. \
                 A browser server list needs an async probe over the ws-web relay.",
                entry.host
            ))
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    Arc::new(move |entry: &ServerEntry| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("could not start a runtime: {e}"))?;
        let s = rt
            .block_on(lodestone_net::server_status(
                &entry.host,
                entry.port,
                protocol,
            ))
            .map_err(|e| e.to_string())?;
        let players = s.players_line();
        Ok(ServerStatus {
            motd: s.motd,
            motd_spans: s.motd_spans,
            players,
            online: s.online,
            sample: s.sample.iter().map(|p| p.name.clone()).collect(),
            version: s.version.unwrap_or_default(),
            protocol: s.protocol,
            favicon_png: s.favicon_png,
            latency_ms: s.latency_ms,
        })
    })
}

/// A probe that refuses to ping, with a self-describing reason.
///
/// Kept for tests and for any build where the menu must not open sockets. The
/// reason is explicit because a row that silently shows nothing is
/// indistinguishable from a server that is down.
#[must_use]
pub fn unavailable_probe() -> Probe {
    Arc::new(|_| Err("status ping disabled in this build".to_string()))
}

/// Cache of per-address status results, with background probing.
pub struct StatusCache {
    slots: HashMap<String, StatusSlot>,
    tx: Sender<(String, Result<ServerStatus, String>)>,
    rx: Receiver<(String, Result<ServerStatus, String>)>,
    probe: Probe,
    /// When this cache was built, i.e. the zero of [`Self::millis`].
    started: crate::platform::Instant,
}

impl std::fmt::Debug for StatusCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatusCache")
            .field("slots", &self.slots)
            .finish_non_exhaustive()
    }
}

impl Default for StatusCache {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusCache {
    /// A cache wired to the **real** status ping ([`net_probe`]).
    ///
    /// The working probe is the default on purpose. A constructor that returned
    /// a do-nothing cache and relied on every caller remembering
    /// [`set_probe`](Self::set_probe) is exactly how a finished subsystem ends up
    /// reaching zero pixels.
    #[must_use]
    pub fn new() -> Self {
        Self::with_probe(net_probe(STATUS_PROTOCOL))
    }

    /// A cache using `probe` instead of the network.
    #[must_use]
    pub fn with_probe(probe: Probe) -> Self {
        let (tx, rx) = channel();
        Self {
            slots: HashMap::new(),
            tx,
            rx,
            probe,
            started: crate::platform::Instant::now(),
        }
    }

    /// Milliseconds since this cache was built — the clock
    /// [`pinging_sprite`]'s animation runs on.
    ///
    /// Vanilla reads `Util.getMillis()`, a process-wide monotonic clock. The
    /// origin does not matter (the animation is `& 7` of a tenth-of-a-second
    /// counter, so any offset only changes which frame a row starts on), and
    /// hanging it off the cache keeps the clock out of `render::frame_for`'s
    /// signature: the frame builder already takes a `&StatusCache`, so nothing
    /// in `app.rs` has to change to make a row animate.
    #[must_use]
    pub fn millis(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Installs the real probe. See the module docs for the implementation.
    pub fn set_probe(&mut self, probe: Probe) {
        self.probe = probe;
    }

    /// The cache key for an entry: its dialable address.
    #[must_use]
    pub fn key(entry: &ServerEntry) -> String {
        format!("{}:{}", entry.host, entry.effective_port())
    }

    /// The slot for `entry`, defaulting to [`StatusSlot::Idle`].
    #[must_use]
    pub fn get(&self, entry: &ServerEntry) -> &StatusSlot {
        self.slots.get(&Self::key(entry)).unwrap_or(&StatusSlot::Idle)
    }

    /// Starts probes for every entry that is not already resolved or in flight.
    ///
    /// Idempotent: calling it every frame does **not** spawn a thread per frame.
    pub fn refresh(&mut self, entries: &[ServerEntry]) {
        for entry in entries {
            let key = Self::key(entry);
            if self.slots.contains_key(&key) {
                continue;
            }
            self.spawn(key, entry.clone());
        }
    }

    /// Discards every cached result for `entries` and probes them all again —
    /// what the player asked for with F5 or the Refresh button (#396).
    ///
    /// Not the same as [`Self::refresh`], and the difference is the whole reason
    /// this exists: that one *skips* any address it already has a result for, so
    /// using it for a refresh would leave every row exactly as it was. Vanilla
    /// gets the same effect by discarding the screen and rebuilding it with a
    /// fresh `ServerList`, whose entries all start in `State.INITIAL`
    /// (`JoinMultiplayerScreen.java:167-169`).
    pub fn refresh_all(&mut self, entries: &[ServerEntry]) {
        for entry in entries {
            self.refresh_one(entry);
        }
    }

    /// Forces a re-probe of one entry, discarding any cached result.
    pub fn refresh_one(&mut self, entry: &ServerEntry) {
        let key = Self::key(entry);
        if self.slots.get(&key).is_some_and(StatusSlot::is_pending) {
            return;
        }
        self.slots.remove(&key);
        self.spawn(key, entry.clone());
    }

    /// Drops any cached slot for `entry` (e.g. after it was edited or deleted).
    pub fn forget(&mut self, entry: &ServerEntry) {
        self.slots.remove(&Self::key(entry));
    }

    fn spawn(&mut self, key: String, entry: ServerEntry) {
        self.slots.insert(key.clone(), StatusSlot::Pending);
        let tx = self.tx.clone();
        let probe = Arc::clone(&self.probe);
        // Detached: the result is delivered through the channel, and a dropped
        // receiver simply makes the send fail. Nothing joins these, so a slow
        // DNS lookup can never stall shutdown.
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(move || {
            let out = probe(&entry);
            let _ = tx.send((key, out));
        });

        // Browser: run the probe inline and send on this thread.
        //
        // **This was a reachable crash, not a latent one.** `std::thread::spawn`
        // TRAPS on wasm32 — measured, executed in a wasm VM: `RuntimeError:
        // unreachable`, and with `panic = "abort"` that is the tab dying — and this
        // function runs when the player opens the Multiplayer screen, via
        // `refresh_one`/`pump`. Nothing about it was visible to any `cargo check`.
        //
        // Inline is correct rather than a compromise: `net_probe`'s browser arm
        // performs **no I/O at all** (a page has no raw TCP socket, so it returns an
        // explanatory `Err` immediately), so there is nothing to move off the frame
        // thread. The channel round-trip is kept so `pump` remains the single place a
        // result reaches a slot — one code path for both targets, and the `Pending`
        // → `Failed` transition a caller observes is identical.
        //
        // If a browser probe ever really does I/O — an async ping over the `ws-web`
        // relay — it belongs in `spawn_local`, and the `tx` clone above is already
        // the right handoff for it. That is a new probe, not a change here.
        #[cfg(target_arch = "wasm32")]
        {
            let out = probe(&entry);
            let _ = tx.send((key, out));
        }
    }

    /// Moves any finished probes into their slots. Call once per frame.
    ///
    /// Returns how many results landed, which is what lets a caller (or a test)
    /// tell "nothing finished yet" from "nothing was ever started".
    pub fn pump(&mut self) -> usize {
        let mut n = 0;
        while let Ok((key, result)) = self.rx.try_recv() {
            let slot = match result {
                Ok(s) => StatusSlot::Ok(Box::new(s)),
                Err(e) => StatusSlot::Failed(e),
            };
            self.slots.insert(key, slot);
            n += 1;
        }
        n
    }

    /// Number of cached slots, for tests and diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether nothing has been probed yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(host: &str, port: Option<u16>) -> ServerEntry {
        ServerEntry::new(host, host, port)
    }

    /// Blocks until `cache.pump()` has delivered `want` results, or the deadline
    /// passes. Returns how many landed.
    fn drain(cache: &mut StatusCache, want: usize) -> usize {
        let deadline = crate::platform::Instant::now() + std::time::Duration::from_secs(5);
        let mut got = 0;
        while got < want && crate::platform::Instant::now() < deadline {
            got += cache.pump();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        got
    }

    #[test]
    fn a_probe_result_reaches_the_slot() {
        let mut cache = StatusCache::with_probe(unavailable_probe());
        cache.set_probe(Arc::new(|e: &ServerEntry| {
            Ok(ServerStatus {
                motd: format!("hello {}", e.host),
                players: "1/20".into(),
                ..Default::default()
            })
        }));

        let e = entry("a.example", None);
        cache.refresh(std::slice::from_ref(&e));
        assert!(cache.get(&e).is_pending(), "refresh must mark it in flight");

        assert_eq!(drain(&mut cache, 1), 1, "probe result never arrived");
        match cache.get(&e) {
            StatusSlot::Ok(s) => {
                assert_eq!(s.motd, "hello a.example");
                assert_eq!(s.players, "1/20");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
        assert!(cache.get(&e).summary().contains("hello a.example"));
    }

    #[test]
    fn a_failing_probe_shows_its_reason_rather_than_looking_empty() {
        let mut cache = StatusCache::with_probe(unavailable_probe());
        cache.set_probe(Arc::new(|_: &ServerEntry| Err("connection refused".into())));
        let e = entry("dead.example", None);
        cache.refresh(std::slice::from_ref(&e));
        assert_eq!(drain(&mut cache, 1), 1);
        assert_eq!(
            cache.get(&e),
            &StatusSlot::Failed("connection refused".into())
        );
        assert!(cache.get(&e).summary().contains("connection refused"));
    }

    #[test]
    fn refresh_is_idempotent_and_does_not_respawn_per_frame() {
        // The bug this prevents: a thread per row per frame.
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let mut cache = StatusCache::with_probe(unavailable_probe());
        cache.set_probe(Arc::new(move |_: &ServerEntry| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ServerStatus::default())
        }));

        let list = vec![entry("a.example", None), entry("b.example", None)];
        for _ in 0..50 {
            cache.refresh(&list);
        }
        assert_eq!(drain(&mut cache, 2), 2);
        // Pump again after results land, then refresh more — still no respawn.
        for _ in 0..50 {
            cache.refresh(&list);
        }
        cache.pump();
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "one probe per address, regardless of frame count"
        );
    }

    #[test]
    fn refresh_one_reprobes_but_forget_clears() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let mut cache = StatusCache::with_probe(unavailable_probe());
        cache.set_probe(Arc::new(move |_: &ServerEntry| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ServerStatus::default())
        }));

        let e = entry("a.example", None);
        cache.refresh(std::slice::from_ref(&e));
        assert_eq!(drain(&mut cache, 1), 1);
        cache.refresh_one(&e);
        assert_eq!(drain(&mut cache, 1), 1);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);

        cache.forget(&e);
        assert_eq!(cache.get(&e), &StatusSlot::Idle);
        assert!(cache.is_empty());
    }

    #[test]
    fn entries_are_keyed_by_dialable_address_not_by_position() {
        // Reordering or renaming the list must not scramble results.
        let a = ServerEntry::new("first name", "h.example", Some(25565));
        let b = ServerEntry::new("second name", "h.example", None);
        assert_eq!(
            StatusCache::key(&a),
            StatusCache::key(&b),
            "an implicit port dials the same address as an explicit 25565"
        );
        let c = ServerEntry::new("n", "h.example", Some(25566));
        assert_ne!(StatusCache::key(&a), StatusCache::key(&c));
    }

    #[test]
    fn a_refused_probe_names_its_reason_rather_than_looking_empty() {
        // A row that silently shows nothing is indistinguishable from a server
        // that is down, so even the refusal path must say why.
        let mut cache = StatusCache::with_probe(unavailable_probe());
        let e = entry("a.example", None);
        cache.refresh(std::slice::from_ref(&e));
        assert_eq!(drain(&mut cache, 1), 1);
        match cache.get(&e) {
            StatusSlot::Failed(why) => {
                assert!(!why.trim().is_empty(), "a blank reason is no reason");
                assert!(why.contains("disabled"), "unhelpful reason: {why}");
            }
            other => panic!("expected an explicit failure, got {other:?}"),
        }
    }

    /// The default cache must be wired to the network, not to a stub. The
    /// evidence is that it *tries*: pointing it at a port nothing listens on
    /// yields a real transport failure rather than "disabled".
    ///
    /// Loopback port 1 is used rather than an off-machine address on purpose —
    /// a refused connection returns immediately, whereas an unroutable address
    /// would sit in the OS connect timeout (75 s on macOS) and make this a slow,
    /// flaky test. This is the gate that would have caught the island: with the
    /// stub installed as the default, it fails.
    #[test]
    fn the_default_cache_actually_uses_the_network() {
        let mut cache = StatusCache::new();
        let e = ServerEntry::new("nothing-there", "127.0.0.1", Some(1));
        cache.refresh(std::slice::from_ref(&e));
        assert_eq!(drain(&mut cache, 1), 1, "the probe never reported back");
        match cache.get(&e) {
            StatusSlot::Failed(why) => {
                let lower = why.to_ascii_lowercase();
                assert!(
                    !lower.contains("disabled"),
                    "the default probe is the stub, so nothing pings: {why}"
                );
                assert!(
                    !lower.contains("runtime"),
                    "the probe never got as far as a socket: {why}"
                );
            }
            StatusSlot::Ok(s) => panic!("TEST-NET-1 answered a status ping: {s:?}"),
            other => panic!("expected a failed probe, got {other:?}"),
        }
    }

    #[test]
    fn pump_distinguishes_nothing_finished_from_nothing_started() {
        let mut cache = StatusCache::new();
        assert_eq!(cache.pump(), 0, "no probes started");
        assert!(cache.is_empty());
    }

    /// The buckets are `< 150 / < 300 / < 600 / < 1000 / else`
    /// (`ServerSelectionList.java:417-427`), and they run *downward*: five bars
    /// for a fast server. The boundary values are the point — a `<=` instead of
    /// a `<` moves every one of them by a millisecond, which no "a ping bar
    /// drew" assertion can see.
    #[test]
    fn the_latency_buckets_are_vanillas_own_and_run_downward() {
        assert_eq!(ping_sprite(Some(0)), "server_list/ping_5");
        assert_eq!(ping_sprite(Some(149)), "server_list/ping_5");
        assert_eq!(ping_sprite(Some(150)), "server_list/ping_4");
        assert_eq!(ping_sprite(Some(299)), "server_list/ping_4");
        assert_eq!(ping_sprite(Some(300)), "server_list/ping_3");
        assert_eq!(ping_sprite(Some(599)), "server_list/ping_3");
        assert_eq!(ping_sprite(Some(600)), "server_list/ping_2");
        assert_eq!(ping_sprite(Some(999)), "server_list/ping_2");
        assert_eq!(ping_sprite(Some(1000)), "server_list/ping_1");
        assert_eq!(ping_sprite(Some(30_000)), "server_list/ping_1");
        // `serverData.ping` is a primitive `long`, so "no measurement" is 0 in
        // vanilla and reads as the fastest bucket. Reproduced, not corrected.
        assert_eq!(ping_sprite(None), "server_list/ping_5");
    }

    /// `(millis / 100 + index * 2) & 7` folded with `if idx > 4 { idx = 8 - idx }`
    /// (`:316-326`). The fold is what makes it a ping-pong rather than a
    /// sawtooth, and the `index * 2` is what puts adjacent rows out of phase —
    /// both asserted, because dropping either still animates.
    #[test]
    fn the_pinging_animation_ping_pongs_and_is_out_of_phase_per_row() {
        // One full period of eight tenths of a second, row 0.
        let frames: Vec<&str> = (0..8).map(|i| pinging_sprite(i * 100, 0)).collect();
        assert_eq!(
            frames,
            vec![
                "server_list/pinging_1",
                "server_list/pinging_2",
                "server_list/pinging_3",
                "server_list/pinging_4",
                "server_list/pinging_5",
                "server_list/pinging_4",
                "server_list/pinging_3",
                "server_list/pinging_2",
            ],
            "the fold must mirror frames 5..7 back down, not wrap to 6/7/8"
        );
        // It is a cycle: the ninth tenth is the first again.
        assert_eq!(pinging_sprite(800, 0), pinging_sprite(0, 0));
        // Row 1 is two frames ahead of row 0 at the same instant, so a list
        // never shows one flat wall of identical bars.
        assert_eq!(pinging_sprite(0, 1), pinging_sprite(200, 0));
        assert_ne!(pinging_sprite(0, 1), pinging_sprite(0, 0));
        // Sub-100ms jitter must not move it: the clock is tenths of a second.
        assert_eq!(pinging_sprite(99, 0), pinging_sprite(0, 0));
        // And it must not panic or wrap oddly for a long-running process or a
        // long list.
        assert!(PINGING_SPRITES.contains(&pinging_sprite(u64::MAX, usize::MAX)));
    }

    /// The four presentations resolve to **four distinct** sprites, which is the
    /// assertion a "something drew a ping bar" gate cannot make.
    #[test]
    fn each_state_resolves_to_its_own_sprite() {
        let ok = |protocol: Option<i32>, latency: Option<u64>| {
            StatusSlot::Ok(Box::new(ServerStatus {
                protocol,
                latency_ms: latency,
                ..Default::default()
            }))
        };
        let good = ok(Some(STATUS_PROTOCOL), Some(10));
        let old = ok(Some(STATUS_PROTOCOL - 1), Some(10));
        let vague = ok(None, Some(10));

        assert_eq!(good.state(STATUS_PROTOCOL), ServerState::Successful);
        assert_eq!(old.state(STATUS_PROTOCOL), ServerState::Incompatible);
        assert_eq!(
            vague.state(STATUS_PROTOCOL),
            ServerState::Incompatible,
            "vanilla's `serverData.protocol` defaults to 0, so an absent \
             protocol is a mismatch there too"
        );
        assert_eq!(StatusSlot::Idle.state(STATUS_PROTOCOL), ServerState::Initial);
        assert_eq!(
            StatusSlot::Pending.state(STATUS_PROTOCOL),
            ServerState::Pinging
        );
        assert_eq!(
            StatusSlot::Failed("no".into()).state(STATUS_PROTOCOL),
            ServerState::Unreachable
        );

        let sprite = |slot: &StatusSlot, latency| {
            status_sprite(slot.state(STATUS_PROTOCOL), latency, 0, 0)
        };
        let reachable = sprite(&good, Some(10));
        let incompatible = sprite(&old, Some(10));
        let unreachable = sprite(&StatusSlot::Failed("no".into()), None);
        let pending = sprite(&StatusSlot::Pending, None);
        let mut all = vec![reachable, incompatible, unreachable, pending];
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 4, "two states share a sprite: {all:?}");
        assert_eq!(reachable, "server_list/ping_5");
        assert_eq!(incompatible, INCOMPATIBLE_SPRITE);
        assert_eq!(unreachable, UNREACHABLE_SPRITE);
        assert_eq!(pending, "server_list/pinging_1");
        // `refreshStatus` gives INITIAL a *static* `ping_1`, not an animation
        // frame — the animation only starts once the state is PINGING (`:402-406`
        // vs `:315-327`).
        assert_eq!(sprite(&StatusSlot::Idle, None), "server_list/ping_1");
    }

    /// Every sprite id this module can name must exist in the pack, or a row
    /// silently draws nothing where its status belongs. The names are checked
    /// against the shape `GuiAtlas` keys on (`server_list/<name>`, no
    /// namespace, no extension) rather than against the atlas itself, which
    /// needs the jar.
    #[test]
    fn every_status_sprite_id_is_a_server_list_sprite() {
        let all = PING_SPRITES
            .iter()
            .chain(PINGING_SPRITES.iter())
            .chain([&INCOMPATIBLE_SPRITE, &UNREACHABLE_SPRITE]);
        let mut n = 0;
        for id in all {
            assert!(id.starts_with("server_list/"), "{id}");
            assert!(!id.ends_with(".png"), "{id}");
            assert!(!id.contains(':'), "{id}");
            n += 1;
        }
        assert_eq!(n, 12, "PING_SPRITES + PINGING_SPRITES + the two states");
    }

    /// The animation needs a clock, and a clock that never advances is a frozen
    /// icon. This asserts the cache's own is monotonic and real.
    #[test]
    fn the_cache_carries_a_clock_for_the_animation() {
        let cache = StatusCache::with_probe(unavailable_probe());
        let first = cache.millis();
        std::thread::sleep(std::time::Duration::from_millis(120));
        let second = cache.millis();
        assert!(second >= first, "the clock went backwards: {first} -> {second}");
        assert!(
            second >= 100,
            "120ms of sleep must move a tenth-of-a-second clock: {second}"
        );
        // And a tenth of a second must move the sprite, or the clock is wired to
        // nothing. Asserted on a *fixed* pair rather than on `second`: the
        // animation is a cycle, so a sleep that overslept to 800 ms would land
        // back on frame 0 and make a correct implementation look frozen.
        assert_ne!(
            pinging_sprite(0, 0),
            pinging_sprite(100, 0),
            "control: 100ms is one animation frame"
        );
    }

    /// #421: the "who's online" tooltip is the sample's names, plus vanilla's
    /// `multiplayer.status.and_more` — "... and N more ..." — when the sample is
    /// short of the reported online count (`ServerStatusPinger.java:90-110`).
    #[test]
    fn player_sample_lines_shapes_the_tooltip() {
        // The full case: 2 of 5 named.
        assert_eq!(
            player_sample_lines(&["Alice".into(), "Bob".into()], Some(5)),
            ["Alice", "Bob", "... and 3 more ..."]
        );
        // Sample exactly the count — no and-more line, matching vanilla's
        // `if (players.sample().size() < players.online())` (`:105`).
        assert_eq!(
            player_sample_lines(&["Alice".into(), "Bob".into()], Some(2)),
            ["Alice", "Bob"]
        );
        // A sample *larger* than the count is the server lying; vanilla trusts
        // the sample and drops the tail line, so this does too.
        assert_eq!(
            player_sample_lines(&["A".into(), "B".into(), "C".into()], Some(1)),
            ["A", "B", "C"]
        );
        // No count reported: names only.
        assert_eq!(
            player_sample_lines(&["Alice".into()], None),
            ["Alice"]
        );
        // No sample: empty — and an empty `playerList` draws no tooltip at all
        // in vanilla (`setTooltipForNextFrameInternal`'s `!lines.isEmpty()`),
        // so "nothing to say" and "no tooltip" are deliberately one value.
        assert_eq!(player_sample_lines(&[], Some(5)), Vec::<String>::new());
    }
}
