//! Shared live-test support helpers.

pub mod bench_fixtures;
pub mod directives;

pub use directives::assert_emits_set;

use std::future::Future;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lodestone_core::Writer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Per-family wire quirks for the hermetic `spawn_entity`/`spawn_entity_living`
/// fixtures shared by `v1-9` and `v1-14`'s `tests/entity.rs`.
///
/// Those two test files are structurally identical seam tests (see each
/// file's own doc comment on why they are nonetheless *not* unified with
/// v1-8's) — comment-stripped, the only difference is six literals: three
/// per-family entity-type wire ids (creeper, arrow, boat), the width of
/// `spawn_entity`'s object-type field (a signed byte pre-1.13, a varint from
/// 1.13 on — `spawn_entity_living`'s mob-type field is a varint in *both*
/// families, so this toggle applies only to the object field), and whether a
/// `0xFF` metadata terminator byte follows the fixed fields (1.13 rewrote the
/// metadata format and dropped it). This table isolates exactly that:
/// parameterize by fixture, not by protocol version number, per this repo's
/// rule that a six-literal delta is a fixture table, not a reason to
/// template the test body.
///
/// Only `v1-9` and `v1-14` use this; `v1-8`'s `tests/entity.rs` is deliberately
/// independent (see its own doc comment) and `v26-2`'s entity wire shape is
/// unrelated to either, so neither participates here.
#[derive(Debug, Clone, Copy)]
pub struct EntitySpawnWireFixture {
    /// Wire id for `minecraft:creeper` in `spawn_entity_living`'s mob-type
    /// field (always a varint — see [`Self::write_mob_type_id`]).
    pub creeper_id: i32,
    /// Wire id for `minecraft:arrow` in `spawn_entity`'s object-type field.
    pub arrow_id: i32,
    /// Wire id for `minecraft:boat` in `spawn_entity`'s object-type field.
    pub boat_id: i32,
    /// Whether `spawn_entity`'s object-type field is a single signed byte
    /// (pre-1.13) rather than a varint (1.13+). Does not affect
    /// `spawn_entity_living`'s mob-type field, which is a varint either way.
    pub object_type_id_is_byte: bool,
    /// Metadata terminator byte appended after a living/named entity spawn's
    /// fixed fields, if this family's decoder still expects one. `None` for
    /// families whose metadata format has no terminator sentinel.
    pub metadata_terminator: Option<u8>,
}

impl EntitySpawnWireFixture {
    /// Writes a `spawn_entity_living` mob-type id. Always a varint in both
    /// families that use this fixture; only the object-type field's width
    /// differs (see [`Self::write_object_type_id`]).
    pub fn write_mob_type_id(&self, w: &mut Writer, id: i32) {
        w.var_i32(id);
    }

    /// Writes a `spawn_entity` object-type id, using this fixture's wire
    /// width.
    pub fn write_object_type_id(&self, w: &mut Writer, id: i32) {
        if self.object_type_id_is_byte {
            w.i8(id as i8);
        } else {
            w.var_i32(id);
        }
    }

    /// Appends this fixture's metadata terminator, if it has one.
    pub fn write_metadata_terminator(&self, w: &mut Writer) {
        if let Some(byte) = self.metadata_terminator {
            w.bytes(&[byte]);
        }
    }
}

const TYPE_AUTH: i32 = 3;
const TYPE_COMMAND: i32 = 2;

static USERNAME_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A per-join offline username, unique **by construction** rather than by clock
/// resolution.
///
/// Offline UUIDs derive from the username, so two joins that share a name are a
/// mutual eviction ("logged in from another location"), not a warning — and a
/// stale corpse under a reused name silently blacks out chunk data. The obvious
/// `nanos % 1e9` is a trap: on macOS `SystemTime` has only microsecond
/// resolution, so every name ends in `000` and the real collision space is ~10⁶,
/// three orders of magnitude below what the expression implies. Two tests
/// running in parallel (the `cargo test` default) then collide and evict each
/// other.
///
/// Instead, uniqueness comes from a process-wide atomic counter (distinct for
/// every call *within* a process, independent of the clock) combined with the pid
/// and a coarse timestamp (distinct *across* processes and runs). The counter is
/// placed first so the hard 16-char server limit can never truncate the
/// in-process discriminator. Every retry/reconnect must call this afresh — never
/// reuse a name across a reconnect, or you evict your own live session.
#[must_use]
pub fn unique_username() -> String {
    let seq = USERNAME_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = u64::from(std::process::id());
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let stamp = base36((secs ^ (pid << 21)) & 0xffff_ffff);
    let mut name = format!("E{}_{}", base36(seq), stamp);
    name.truncate(16);
    name
}

/// Builds a complete Source RCON request frame as one contiguous buffer.
///
/// Vanilla's `RconClient` performs exactly one `read()` per request; splitting a
/// request across multiple socket writes can intermittently make the server
/// close the connection. Callers should write this returned frame with one
/// `write_all`.
#[must_use]
pub fn rcon_frame(id: i32, packet_type: i32, payload: &str) -> Vec<u8> {
    let body_len = 4 + 4 + payload.len() + 2;
    let mut frame = Vec::with_capacity(4 + body_len);
    frame.extend_from_slice(&(body_len as i32).to_le_bytes());
    frame.extend_from_slice(&id.to_le_bytes());
    frame.extend_from_slice(&packet_type.to_le_bytes());
    frame.extend_from_slice(payload.as_bytes());
    frame.extend_from_slice(&[0, 0]);
    frame
}

/// Polls a condition until it returns `Some(T)` or a timeout expires.
///
/// Live tests that create server state (`/summon`, `/kill`, deaths, inventory
/// edits) must poll for the server-visible result instead of asserting
/// immediately; vanilla often publishes the effect on the next tick.
pub async fn poll_until<T, Fut, F>(timeout: Duration, interval: Duration, mut check: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = check().await {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(interval).await;
    }
}

/// Polls a synchronous condition until it returns `Some(T)` or a timeout expires.
///
/// Use this for blocking live-test probes such as RCON selector checks. Server
/// state created by `/summon`, `/kill`, deaths, `item replace`, and similar
/// commands is tick-published; asserting immediately after the command is a
/// timing bug.
pub fn poll_until_blocking<T, F>(timeout: Duration, interval: Duration, mut check: F) -> Option<T>
where
    F: FnMut() -> Option<T>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = check() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(interval);
    }
}

/// Counter for anti-vacuity assertions in loops with optional data.
///
/// Tests that use `Option`-returning helpers and `continue`-heavy loops can pass
/// after comparing zero real cases. Increment this for every meaningful
/// comparison and assert a floor at the end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckCounter {
    label: &'static str,
    count: usize,
}

impl CheckCounter {
    /// Creates a named zero counter.
    #[must_use]
    pub const fn new(label: &'static str) -> Self {
        Self { label, count: 0 }
    }

    /// Records one meaningful check.
    pub fn mark(&mut self) {
        self.add(1);
    }

    /// Records `n` meaningful checks.
    pub fn add(&mut self, n: usize) {
        self.count = self.count.saturating_add(n);
    }

    /// Returns the number of recorded checks.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// Panics if fewer than `minimum` meaningful checks were recorded.
    pub fn assert_at_least(&self, minimum: usize) {
        assert!(
            self.count >= minimum,
            "{} was vacuous: checked {}, expected at least {}",
            self.label,
            self.count,
            minimum
        );
    }
}

/// Selects a fixture by exact file name, never by directory iteration order.
///
/// `read_dir()` order is platform- and filesystem-dependent. Live gates that
/// need a specific generated jar or JSON fixture must name it explicitly so a
/// sibling cache entry cannot silently turn a test into a no-op.
pub fn fixture_by_name(dir: impl AsRef<Path>, file_name: &str) -> std::io::Result<PathBuf> {
    let path = dir.as_ref().join(file_name);
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("fixture is not a file: {}", path.display()),
        ));
    }
    Ok(path)
}

/// Blocking Source RCON client for live tests.
///
/// ## An empty reply body is not proof the command failed
///
/// Both this and [`AsyncRconClient`] read a response with `read_exact` on
/// the declared length, so a short/truncated body cannot reach a caller as a
/// successful, silently-wrong result: `read_exact` either fills the buffer or
/// returns an `io::Error`, never a partial one dressed up as complete. That
/// was the first thing checked when a real oracle run intermittently got a
/// matching-id, empty-body reply back for a `tp` -- and it ruled the read
/// loop out. Tagging a batch of commands under concurrent RCON connections
/// (several `RconClient`s hammering one server at once, the shape a shared
/// live-test oracle sees when more than one gate runs against it) and
/// checking server-side afterward found the command had executed in every
/// case whose reply came back empty; a captured exchange showed the body
/// arriving as a single `read` filling the declared length exactly, with no
/// further bytes ever following it. So the loss is real, but it is server-side
/// and in the confirmation *text*, not in whether the command ran and not in
/// how this client reads the wire.
///
/// The consequence for a caller: do not gate success on the reply string
/// alone for a command whose real effect can be confirmed another way (server
/// state via a follow-up query, or client-observed state for something like a
/// teleport). Treat an empty reply as inconclusive and fall through to that
/// confirmation, the way `crates/lodestone-shell/tests/live/
/// live_edge_back_off_rubber_band.rs`'s `rcon_teleport_and_confirm` does --
/// and do not retry the command itself to paper over an empty reply, since
/// nothing here has shown the command needs re-running.
#[derive(Debug)]
pub struct RconClient {
    stream: TcpStream,
    next_id: i32,
}

impl RconClient {
    /// Connects and authenticates to an RCON endpoint.
    pub fn connect<A: ToSocketAddrs>(addr: A, password: &str) -> std::io::Result<Self> {
        let mut client = Self {
            stream: TcpStream::connect(addr)?,
            next_id: 1,
        };
        let id = client.send(TYPE_AUTH, password)?;
        let (response_id, _) = client.read_response()?;
        if response_id != id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "RCON authentication failed",
            ));
        }
        Ok(client)
    }

    /// Sends one command and returns the response body.
    pub fn command(&mut self, command: &str) -> std::io::Result<String> {
        let id = self.send(TYPE_COMMAND, command)?;
        let (response_id, body) = self.read_response()?;
        if response_id != id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "RCON response id mismatch",
            ));
        }
        Ok(body)
    }

    /// Sends commands sequentially and returns each response body in order.
    pub fn commands<I, S>(&mut self, commands: I) -> std::io::Result<Vec<String>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        commands
            .into_iter()
            .map(|command| self.command(command.as_ref()))
            .collect()
    }

    /// Sends one command and panics if the RCON exchange fails.
    pub fn cmd(&mut self, command: &str) -> String {
        self.command(command).expect("RCON command")
    }

    /// Polls until `selector` is visible to `data get entity`, then returns the
    /// command response.
    ///
    /// This protects tests from the §12.18 tick trap: a freshly summoned entity
    /// is often not selector-visible until a later server tick.
    pub fn wait_for_entity(
        &mut self,
        selector: &str,
        timeout: Duration,
        interval: Duration,
    ) -> std::io::Result<String> {
        poll_until_blocking(timeout, interval, || {
            let response = self.cmd(&format!("data get entity {selector} Pos"));
            if response.contains("No entity was found") {
                None
            } else {
                Some(response)
            }
        })
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("entity {selector} was not visible before timeout"),
            )
        })
    }

    fn send(&mut self, packet_type: i32, payload: &str) -> std::io::Result<i32> {
        let id = self.next_id;
        self.next_id += 1;
        let frame = rcon_frame(id, packet_type, payload);
        self.stream.write_all(&frame)?;
        Ok(id)
    }

    fn read_response(&mut self) -> std::io::Result<(i32, String)> {
        let mut len_buf = [0; 4];
        self.stream.read_exact(&mut len_buf)?;
        let len = i32::from_le_bytes(len_buf);
        if len < 10 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid RCON frame length",
            ));
        }

        let mut body = vec![0; len as usize];
        self.stream.read_exact(&mut body)?;
        decode_rcon_response(&body)
    }
}

/// Async Source RCON client for Tokio live tests.
#[derive(Debug)]
pub struct AsyncRconClient {
    stream: tokio::net::TcpStream,
    next_id: i32,
}

impl AsyncRconClient {
    /// Connects and authenticates to an RCON endpoint.
    pub async fn connect<A: tokio::net::ToSocketAddrs>(
        addr: A,
        password: &str,
    ) -> std::io::Result<Self> {
        let mut client = Self {
            stream: tokio::net::TcpStream::connect(addr).await?,
            next_id: 1,
        };
        let id = client.send(TYPE_AUTH, password).await?;
        let (response_id, _) = client.read_response().await?;
        if response_id != id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "RCON authentication failed",
            ));
        }
        Ok(client)
    }

    /// Sends one command and returns the response body.
    pub async fn command(&mut self, command: &str) -> std::io::Result<String> {
        let id = self.send(TYPE_COMMAND, command).await?;
        let (response_id, body) = self.read_response().await?;
        if response_id != id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "RCON response id mismatch",
            ));
        }
        Ok(body)
    }

    /// Sends commands sequentially and returns each response body in order.
    pub async fn commands<I, S>(&mut self, commands: I) -> std::io::Result<Vec<String>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out = Vec::new();
        for command in commands {
            out.push(self.command(command.as_ref()).await?);
        }
        Ok(out)
    }

    /// Sends one command and panics if the RCON exchange fails.
    pub async fn cmd(&mut self, command: &str) -> String {
        self.command(command).await.expect("RCON command")
    }

    /// Polls until `selector` is visible to `data get entity`, then returns the
    /// command response.
    pub async fn wait_for_entity(
        &mut self,
        selector: &str,
        timeout: Duration,
        interval: Duration,
    ) -> std::io::Result<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let response = self.cmd(&format!("data get entity {selector} Pos")).await;
            if !response.contains("No entity was found") {
                return Ok(response);
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("entity {selector} was not visible before timeout"),
                ));
            }
            tokio::time::sleep(interval).await;
        }
    }

    async fn send(&mut self, packet_type: i32, payload: &str) -> std::io::Result<i32> {
        let id = self.next_id;
        self.next_id += 1;
        let frame = rcon_frame(id, packet_type, payload);
        self.stream.write_all(&frame).await?;
        Ok(id)
    }

    async fn read_response(&mut self) -> std::io::Result<(i32, String)> {
        let mut len_buf = [0; 4];
        self.stream.read_exact(&mut len_buf).await?;
        let len = i32::from_le_bytes(len_buf);
        if len < 10 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid RCON frame length",
            ));
        }

        let mut body = vec![0; len as usize];
        self.stream.read_exact(&mut body).await?;
        decode_rcon_response(&body)
    }
}

fn decode_rcon_response(body: &[u8]) -> std::io::Result<(i32, String)> {
    if body.len() < 10 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "short RCON response",
        ));
    }

    let id = i32::from_le_bytes(body[0..4].try_into().expect("slice length checked"));
    let payload_end = body.len().saturating_sub(2);
    let payload = String::from_utf8_lossy(&body[8..payload_end]).into_owned();
    Ok((id, payload))
}

fn base36(mut n: u64) -> String {
    const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_owned();
    }

    let mut out = Vec::new();
    while n > 0 {
        out.push(ALPHABET[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("base36 alphabet is valid UTF-8")
}
