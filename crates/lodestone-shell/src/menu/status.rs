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
    /// MOTD, already flattened to plain text.
    pub motd: String,
    /// Player count, pre-rendered as `online/max`.
    pub players: String,
    /// Server version name, e.g. `"26.2"`.
    pub version: String,
    /// Favicon PNG bytes, when the server sent a usable one.
    pub favicon_png: Option<Vec<u8>>,
    /// Ping round-trip, in milliseconds.
    pub latency_ms: Option<u64>,
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
#[must_use]
pub fn net_probe(protocol: i32) -> Probe {
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
            players,
            version: s.version.unwrap_or_default(),
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
        }
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
        std::thread::spawn(move || {
            let out = probe(&entry);
            let _ = tx.send((key, out));
        });
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut got = 0;
        while got < want && std::time::Instant::now() < deadline {
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
}
