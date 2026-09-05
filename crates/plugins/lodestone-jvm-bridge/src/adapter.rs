//! Explicit experimental adapter loading and tick dispatch on a dedicated JVM thread.
//!
//! This is a bootstrap contract for developing the Paper host, not a Java plugin
//! discovery mechanism. The supplied class declares `static void onTick(long)`,
//! `static void onBlockStateChanged(int, int, int, int)`,
//! `static void onPlayerJoined(long)`, `static void onPlayerDisconnected(long)`,
//! and `static native int blockStateId(int, int, int)`. Native requests cross
//! the world port; the worker never receives world state. The resident
//! block-change listener may also receive a value-only player identity as an
//! opaque handle for the duration of its callback. A host must service the
//! port and poll completion, and must not dispatch another callback until idle.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::time::{Duration, Instant};

use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{Global, JClass, JObject, JString};
use jni::strings::JNIString;
use jni::sys::{jboolean, jint, jlong, jobject, jstring};
use jni::{Env, EnvUnowned, JValue, NativeMethod, jni_sig, jni_str};

use crate::runtime::{JvmConfig, JvmRuntime};
use crate::{
    CallbackDepthGuard, ObjectKind, ObjectRef, ObjectRegistry, PortServicer, ResolveError,
    WorldPort, channel,
};

/// A block query in the host's primary world, in absolute block coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockStateQuery {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// A host must distinguish an unavailable position from a valid air state.
pub type BlockStateAnswer = Result<u32, String>;

/// A requested replacement of one already-resident primary-world block.
///
/// `state_id` is deliberately still raw at this JNI-facing boundary. The
/// native host validates it against the server's generated state table before
/// mutating terrain; this crate does not name a game-data crate merely to
/// duplicate that validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockStateWrite {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub state_id: u32,
}

/// The value-only identity of the player associated with one host-confirmed
/// callback.
///
/// This is deliberately not an ECS entity, connection, or borrowed server
/// object. The host supplies the stable profile bytes and display name, and
/// the adapter turns that value into an opaque, generation-checked handle for
/// the listener callback. A reconnect can therefore supply a new identity
/// without making an old Java `long` point at the replacement player.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PlayerIdentity {
    uuid: [u8; 16],
    name: String,
}

impl PlayerIdentity {
    /// Creates a player identity from its stable profile bytes and name.
    #[must_use]
    pub fn new(uuid: [u8; 16], name: impl Into<String>) -> Self {
        Self {
            uuid,
            name: name.into(),
        }
    }

    /// The stable profile bytes used to distinguish reconnects and players
    /// with the same display name.
    #[must_use]
    pub const fn uuid(&self) -> [u8; 16] {
        self.uuid
    }

    /// The host-authored display name exposed by the narrow fixture query.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A failed write must not be reported as a successful no-op.
pub type BlockStateWriteAnswer = Result<(), String>;

type BlockPort = WorldPort<BlockStateQuery, BlockStateAnswer>;
type BlockWritePort = WorldPort<BlockStateWrite, BlockStateWriteAnswer>;
type TickPort = WorldPort<(), ServerTickAnswer>;
type Events = SyncSender<Result<AdapterEvent, AdapterError>>;

/// A host must distinguish an inactive game tick from a valid count.
pub type ServerTickAnswer = Result<u64, String>;

/// Proof that setup is running beside live native server-state producers.
///
/// Only [`AdapterHost`] creates this token, from the worker's request ports.
/// A lifecycle setup may consume it to install a loader-local Java surface,
/// but cannot use it to read world state directly. The matching
/// [`AdapterHost::service_pending`] and
/// [`AdapterHost::service_pending_server_tick`] endpoints remain the only
/// producers.
#[derive(Debug)]
pub struct NativeServerSurface {
    _block_port: BlockPort,
    _block_write_port: BlockWritePort,
    _tick_port: TickPort,
}

impl NativeServerSurface {
    fn from_ports(block_port: BlockPort, block_write_port: BlockWritePort, tick_port: TickPort) -> Self {
        Self {
            _block_port: block_port,
            _block_write_port: block_write_port,
            _tick_port: tick_port,
        }
    }
}

thread_local! {
    static CALLBACK_PORT: RefCell<Option<BlockPort>> = const { RefCell::new(None) };
    static BLOCK_WRITE_PORT: RefCell<Option<BlockWritePort>> = const { RefCell::new(None) };
    static SERVER_TICK_PORT: RefCell<Option<TickPort>> = const { RefCell::new(None) };
    static RESIDENT_OBJECT_HANDLES: RefCell<Option<ObjectRegistry<ResidentObject>>> = const {
        RefCell::new(None)
    };
    static CURRENT_RESIDENT_BLOCK_HANDLE: RefCell<Option<ObjectRef>> = const { RefCell::new(None) };
    static CURRENT_RESIDENT_PLAYER_HANDLE: RefCell<Option<ObjectRef>> = const { RefCell::new(None) };
    /// Handles for players currently reported by the host's connection
    /// registry. Unlike callback-scoped handles, these survive a join callback
    /// until the matching disconnect callback releases them.
    static ACTIVE_PLAYER_HANDLES: RefCell<Option<HashMap<PlayerIdentity, ObjectRef>>> = const {
        RefCell::new(None)
    };
    static LIFECYCLE_IDENTITY: RefCell<Vec<LifecycleIdentity>> = const {
        RefCell::new(Vec::new())
    };
    static RESIDENT_BLOCK_CHANGE_SUBSCRIPTIONS: RefCell<Option<ResidentBlockChangeSubscriptions<Global<JObject<'static>>>>> = const {
        RefCell::new(None)
    };
}

/// Descriptor identity available only during one retained-entry call.
///
/// This is deliberately worker-local rather than a JVM property or a static
/// field: another plugin cannot observe it between callbacks, and nested Java
/// calls restore the outer identity when they return.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct LifecycleIdentity {
    name: String,
    version: String,
    main_class: String,
}

/// One listener failure reported after a resident block-state callback.
///
/// The host-confirmed change has already happened. This report means one
/// isolated listener failed while observing it; it does not cancel the change
/// or prevent later registrations from receiving that same callback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidentBlockChangeListenerFailure {
    /// Zero-based registration number assigned when the listener subscribed.
    pub registration: usize,
    /// Validated descriptor name captured while the listener subscribed.
    pub plugin_name: String,
    /// Bounded Java failure description after the listener exception was cleared.
    pub detail: String,
}

/// Maximum number of resident block-change listeners retained by one worker.
///
/// A plugin can register more than once, so this is a worker-wide bound rather
/// than a per-plugin promise. It keeps registration aligned with the bounded
/// request ports: a misbehaving entry gets a named error instead of growing a
/// process-lifetime collection without limit.
const MAX_RESIDENT_BLOCK_CHANGE_SUBSCRIPTIONS: usize = 64;

/// Maximum number of live object references retained by one adapter worker.
///
/// References are worker-local values, so this bound covers all retained block
/// and player entries on that worker. Releasing an entry's references returns
/// the slots to the same generation-safe registry for reuse.
pub const MAX_RESIDENT_OBJECT_HANDLES: usize = 1024;

/// Compatibility name for the original block-handle budget.
pub const MAX_RESIDENT_BLOCK_HANDLES: usize = MAX_RESIDENT_OBJECT_HANDLES;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ResidentObject {
    Block {
        owner: LifecycleIdentity,
        position: (i32, i32, i32),
    },
    Player {
        owner: LifecycleIdentity,
        player: PlayerIdentity,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResidentBlockChangeRegistration {
    number: usize,
    identity: LifecycleIdentity,
    active: bool,
}

struct ResidentBlockChangeSubscription<T> {
    registration: ResidentBlockChangeRegistration,
    listener: T,
}

/// Ordered listener bookkeeping shared by the JNI storage and hermetic tests.
///
/// The order is insertion order, never a hash-map order. Registrations begin
/// inactive so constructor and enable failures cannot leak listeners into a
/// later host-confirmed change; the lifecycle owner activates them only after
/// the matching enable callback succeeds.
#[derive(Default)]
struct ResidentBlockChangeSubscriptions<T> {
    entries: Vec<ResidentBlockChangeSubscription<T>>,
    next_registration: usize,
}

impl<T> ResidentBlockChangeSubscriptions<T> {
    fn register(&mut self, identity: LifecycleIdentity, listener: T) -> Result<usize, AdapterError> {
        if self.entries.len() >= MAX_RESIDENT_BLOCK_CHANGE_SUBSCRIPTIONS {
            return Err(AdapterError::new(format!(
                "resident block listener subscription limit {} exceeded",
                MAX_RESIDENT_BLOCK_CHANGE_SUBSCRIPTIONS,
            )));
        }
        let registration = self.next_registration;
        self.next_registration += 1;
        self.entries.push(ResidentBlockChangeSubscription {
            registration: ResidentBlockChangeRegistration {
                number: registration,
                identity,
                active: false,
            },
            listener,
        });
        Ok(registration)
    }

    fn activate(&mut self, identity: &LifecycleIdentity) -> usize {
        let mut activated = 0;
        for entry in &mut self.entries {
            if entry.registration.identity == *identity && !entry.registration.active {
                entry.registration.active = true;
                activated += 1;
            }
        }
        activated
    }

    fn clear(&mut self, identity: &LifecycleIdentity) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|entry| entry.registration.identity != *identity);
        before - self.entries.len()
    }

    fn active_entries(&self) -> impl Iterator<Item = (usize, &LifecycleIdentity, &T)> {
        self.entries
            .iter()
            .filter(|entry| entry.registration.active)
            .map(|entry| {
                (
                    entry.registration.number,
                    &entry.registration.identity,
                    &entry.listener,
                )
            })
    }
}

fn dispatch_isolated_listeners<T>(
    listeners: impl IntoIterator<Item = (usize, String, T)>,
    mut invoke: impl FnMut(&T) -> Result<(), String>,
) -> Vec<ResidentBlockChangeListenerFailure> {
    let mut failures = Vec::new();
    for (registration, plugin_name, listener) in listeners {
        if let Err(detail) = invoke(&listener) {
            failures.push(ResidentBlockChangeListenerFailure {
                registration,
                plugin_name,
                detail,
            });
        }
    }
    failures
}

struct LifecycleIdentityGuard;

/// Restores the previous callback handle when a listener returns, including
/// when it recursively calls back into Java.
struct ResidentBlockHandleGuard(Option<ObjectRef>);

impl ResidentBlockHandleGuard {
    fn enter(handle: Option<ObjectRef>) -> Self {
        let previous = CURRENT_RESIDENT_BLOCK_HANDLE.with(|slot| slot.replace(handle));
        Self(previous)
    }
}

impl Drop for ResidentBlockHandleGuard {
    fn drop(&mut self) {
        CURRENT_RESIDENT_BLOCK_HANDLE.with(|slot| {
            slot.replace(self.0.take());
        });
    }
}

/// Restores the previous callback player when a listener returns, including
/// when it recursively calls back into Java.
struct ResidentPlayerHandleGuard(Option<ObjectRef>);

impl ResidentPlayerHandleGuard {
    fn enter(handle: Option<ObjectRef>) -> Self {
        let previous = CURRENT_RESIDENT_PLAYER_HANDLE.with(|slot| slot.replace(handle));
        Self(previous)
    }
}

impl Drop for ResidentPlayerHandleGuard {
    fn drop(&mut self) {
        CURRENT_RESIDENT_PLAYER_HANDLE.with(|slot| {
            slot.replace(self.0.take());
        });
    }
}

fn current_resident_block_handle() -> Result<ObjectRef, AdapterError> {
    CURRENT_RESIDENT_BLOCK_HANDLE
        .with(|slot| *slot.borrow())
        .ok_or_else(|| {
            AdapterError::new(
                "currentBlockHandle requires an active resident block-change callback",
            )
        })
}

fn current_resident_player_handle() -> Result<ObjectRef, AdapterError> {
    CURRENT_RESIDENT_PLAYER_HANDLE
        .with(|slot| *slot.borrow())
        .ok_or_else(|| {
            AdapterError::new(
                "currentPlayerHandle requires an active resident block-change callback with a player",
            )
        })
}

fn resident_block_handle(
    identity: &LifecycleIdentity,
    change: BlockStateWrite,
) -> Result<ObjectRef, AdapterError> {
    RESIDENT_OBJECT_HANDLES.with(|slot| {
        let mut handles = slot.borrow_mut();
        let handles = handles.as_mut().ok_or_else(|| {
            AdapterError::new("block reference registry requires the adapter worker thread")
        })?;
        handles
            .try_handle_for(
                ObjectKind::Block,
                ResidentObject::Block {
                    owner: identity.clone(),
                    position: (change.x, change.y, change.z),
                },
            )
            .map_err(|error| AdapterError::new(error.to_string()))
    })
}

fn resident_player_handle(
    identity: &LifecycleIdentity,
    player: &PlayerIdentity,
) -> Result<ObjectRef, AdapterError> {
    RESIDENT_OBJECT_HANDLES.with(|slot| {
        let mut handles = slot.borrow_mut();
        let handles = handles.as_mut().ok_or_else(|| {
            AdapterError::new("player reference registry requires the adapter worker thread")
        })?;
        handles
            .try_handle_for(
                ObjectKind::Player,
                ResidentObject::Player {
                    owner: identity.clone(),
                    player: player.clone(),
                },
            )
            .map_err(|error| AdapterError::new(error.to_string()))
    })
}

/// Gets the one worker-owned handle for a currently connected player. The
/// reverse map makes a repeated roster observation idempotent without asking
/// the registry to expose slot indices or pointers.
fn active_player_handle(
    identity: &LifecycleIdentity,
    player: &PlayerIdentity,
) -> Result<ObjectRef, AdapterError> {
    ACTIVE_PLAYER_HANDLES.with(|slot| {
        let mut active = slot.borrow_mut();
        let active = active.as_mut().ok_or_else(|| {
            AdapterError::new("player lifecycle registry requires the adapter worker thread")
        })?;
        if let Some(handle) = active.get(player) {
            return Ok(*handle);
        }
        let handle = resident_player_handle(identity, player)?;
        active.insert(player.clone(), handle);
        Ok(handle)
    })
}

/// Finds the worker-owned handle for one currently connected player. Unknown
/// disconnects are intentionally a no-op: lifecycle cleanup must never mint a
/// new object merely to release it.
fn active_player_handle_for(player: &PlayerIdentity) -> Option<ObjectRef> {
    ACTIVE_PLAYER_HANDLES.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|active| active.get(player).copied())
    })
}

/// Finds the one active player whose copied profile UUID matches a Java
/// lookup. The map remains keyed by the complete value identity so a reconnect
/// with a changed display name still gets a fresh generation; this reverse
/// lookup deliberately refuses an impossible duplicate UUID instead of
/// selecting whichever hash-map entry happens to be visited first.
fn active_player_handle_for_uuid(uuid: [u8; 16]) -> Result<ObjectRef, AdapterError> {
    ACTIVE_PLAYER_HANDLES.with(|slot| {
        let active = slot.borrow();
        let active = active.as_ref().ok_or_else(|| {
            AdapterError::new("playerHandleForUuid requires the adapter worker thread")
        })?;
        let mut matches = active
            .iter()
            .filter(|(player, _)| player.uuid() == uuid)
            .map(|(_, handle)| *handle);
        let Some(handle) = matches.next() else {
            return Err(AdapterError::new(format!(
                "playerHandleForUuid: no active player with UUID {}",
                canonical_uuid_string(uuid),
            )));
        };
        if matches.next().is_some() {
            return Err(AdapterError::new(format!(
                "playerHandleForUuid: multiple active players with UUID {}",
                canonical_uuid_string(uuid),
            )));
        }
        Ok(handle)
    })
}

/// Finds one active player by its copied display name.
///
/// A display name is not globally unique, so this deliberately rejects more
/// than one match instead of selecting a hash-map iteration winner. The host
/// roster remains the source of these values; JNI never reads a connection or
/// server registry to resolve a name.
fn active_player_handle_for_name(name: &str) -> Result<ObjectRef, AdapterError> {
    ACTIVE_PLAYER_HANDLES.with(|slot| {
        let active = slot.borrow();
        let active = active.as_ref().ok_or_else(|| {
            AdapterError::new("playerHandleForName requires the adapter worker thread")
        })?;
        let mut matches = active
            .iter()
            .filter(|(player, _)| player.name() == name)
            .map(|(_, handle)| *handle);
        let Some(handle) = matches.next() else {
            return Err(AdapterError::new(format!(
                "playerHandleForName: no active player named {name:?}",
            )));
        };
        if matches.next().is_some() {
            return Err(AdapterError::new(format!(
                "playerHandleForName: multiple active players named {name:?}",
            )));
        }
        Ok(handle)
    })
}

/// Finds one active player by an ASCII case-insensitive copied display name.
///
/// The explicit ASCII contract avoids inventing locale-dependent Unicode
/// folding at the JNI boundary. As with exact-name lookup, collisions fail
/// rather than letting hash-map iteration choose a player for Java.
fn active_player_handle_for_name_ignoring_case(
    name: &str,
) -> Result<ObjectRef, AdapterError> {
    if !name.is_ascii() {
        return Err(AdapterError::new(format!(
            "playerHandleForNameIgnoringCase: invalid non-ASCII player name {name:?}",
        )));
    }
    ACTIVE_PLAYER_HANDLES.with(|slot| {
        let active = slot.borrow();
        let active = active.as_ref().ok_or_else(|| {
            AdapterError::new("playerHandleForNameIgnoringCase requires the adapter worker thread")
        })?;
        let mut matches = active
            .iter()
            .filter(|(player, _)| {
                player.name().is_ascii() && player.name().eq_ignore_ascii_case(name)
            })
            .map(|(_, handle)| *handle);
        let Some(handle) = matches.next() else {
            return Err(AdapterError::new(format!(
                "playerHandleForNameIgnoringCase: no active player named {name:?}",
            )));
        };
        if matches.next().is_some() {
            return Err(AdapterError::new(format!(
                "playerHandleForNameIgnoringCase: multiple active players named {name:?}",
            )));
        }
        Ok(handle)
    })
}

/// Finds one active player by a non-empty copied display-name prefix.
///
/// Prefix lookup is useful only when it has one answer. It deliberately
/// rejects every collision rather than letting the worker's hash-map order
/// decide which active player Java receives.
fn active_player_handle_for_name_prefix(prefix: &str) -> Result<ObjectRef, AdapterError> {
    if prefix.is_empty() {
        return Err(AdapterError::new(
            "playerHandleForNamePrefix requires a non-empty player name prefix",
        ));
    }
    ACTIVE_PLAYER_HANDLES.with(|slot| {
        let active = slot.borrow();
        let active = active.as_ref().ok_or_else(|| {
            AdapterError::new("playerHandleForNamePrefix requires the adapter worker thread")
        })?;
        let mut matches = active
            .iter()
            .filter(|(player, _)| player.name().starts_with(prefix))
            .map(|(_, handle)| *handle);
        let Some(handle) = matches.next() else {
            return Err(AdapterError::new(format!(
                "playerHandleForNamePrefix: no active player whose name starts with {prefix:?}",
            )));
        };
        if matches.next().is_some() {
            return Err(AdapterError::new(format!(
                "playerHandleForNamePrefix: multiple active players whose names start with {prefix:?}",
            )));
        }
        Ok(handle)
    })
}

/// Finds a connected player by the complete copied profile value.
///
/// This is the disambiguating inverse for a roster that contains duplicate
/// display names or duplicate UUIDs while a reconnect is being reconciled.
/// The profile pair is the worker map key, so it cannot depend on hash-map
/// iteration order and never consults a connection or server registry.
fn active_player_handle_for_profile(
    name: &str,
    uuid: [u8; 16],
) -> Result<ObjectRef, AdapterError> {
    ACTIVE_PLAYER_HANDLES.with(|slot| {
        let active = slot.borrow();
        let active = active.as_ref().ok_or_else(|| {
            AdapterError::new("playerHandleForProfile requires the adapter worker thread")
        })?;
        active
            .get(&PlayerIdentity::new(uuid, name))
            .copied()
            .ok_or_else(|| {
                AdapterError::new(format!(
                    "playerHandleForProfile: no active player named {name:?} with UUID {}",
                    canonical_uuid_string(uuid),
                ))
            })
    })
}

/// Returns the worker-owned handle at one deterministic position in the
/// reconciled active-player snapshot.
///
/// A count without an enumeration path cannot represent the usual server
/// operation of walking the online-player set. The backing map is keyed by a
/// complete copied profile, so iteration order is not a contract; sort by
/// UUID and then display name before selecting the requested entry. Negative
/// and out-of-range positions fail before a handle can be fabricated.
fn active_player_handle_at(index: jint) -> Result<ObjectRef, AdapterError> {
    let index = usize::try_from(index)
        .map_err(|_| AdapterError::new("activePlayerHandleAt requires a non-negative index"))?;
    ACTIVE_PLAYER_HANDLES.with(|slot| {
        let active = slot.borrow();
        let active = active.as_ref().ok_or_else(|| {
            AdapterError::new("activePlayerHandleAt requires the adapter worker thread")
        })?;
        let mut entries = active.iter().collect::<Vec<_>>();
        entries.sort_unstable_by(|(left, _), (right, _)| {
            left.uuid()
                .cmp(&right.uuid())
                .then_with(|| left.name().cmp(right.name()))
        });
        entries
            .get(index)
            .map(|(_, handle)| **handle)
            .ok_or_else(|| {
                AdapterError::new(format!(
                    "activePlayerHandleAt: index {index} is outside active player count {}",
                    entries.len(),
                ))
            })
    })
}

fn parse_uuid_string(value: &str, operation: &str) -> Result<[u8; 16], AdapterError> {
    let bytes = value.as_bytes();
    if bytes.len() != 36 || ![8, 13, 18, 23].into_iter().all(|index| bytes[index] == b'-') {
        return Err(AdapterError::new(format!(
            "{operation}: invalid UUID {value:?} (expected 36-character form)",
        )));
    }
    let mut uuid = [0; 16];
    let mut output = 0;
    let mut index = 0;
    while index < bytes.len() {
        if matches!(index, 8 | 13 | 18 | 23) {
            index += 1;
            continue;
        }
        let high = hex_digit(bytes[index]).ok_or_else(|| {
            AdapterError::new(format!(
                "{operation}: invalid UUID {value:?} (non-hex digit)",
            ))
        })?;
        let low = hex_digit(bytes[index + 1]).ok_or_else(|| {
            AdapterError::new(format!(
                "{operation}: invalid UUID {value:?} (non-hex digit)",
            ))
        })?;
        uuid[output] = (high << 4) | low;
        output += 1;
        index += 2;
    }
    Ok(uuid)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn canonical_uuid_string(uuid: [u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(36);
    for (index, byte) in uuid.into_iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        text.push(HEX[usize::from(byte >> 4)] as char);
        text.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    text
}

fn resolve_active_player_uuid(value: &str) -> Result<ObjectRef, AdapterError> {
    active_player_handle_for_uuid(parse_uuid_string(value, "playerHandleForUuid")?)
}

fn resolve_active_player_name(value: Option<&str>) -> Result<ObjectRef, AdapterError> {
    let name = value.ok_or_else(|| {
        AdapterError::new("playerHandleForName requires a player name")
    })?;
    active_player_handle_for_name(name)
}

fn resolve_active_player_name_ignoring_case(
    value: Option<&str>,
) -> Result<ObjectRef, AdapterError> {
    let name = value.ok_or_else(|| {
        AdapterError::new("playerHandleForNameIgnoringCase requires a player name")
    })?;
    active_player_handle_for_name_ignoring_case(name)
}

fn resolve_active_player_name_prefix(value: Option<&str>) -> Result<ObjectRef, AdapterError> {
    let prefix = value.ok_or_else(|| {
        AdapterError::new("playerHandleForNamePrefix requires a player name prefix")
    })?;
    active_player_handle_for_name_prefix(prefix)
}

fn resolve_active_player_profile(
    name: Option<&str>,
    uuid: Option<&str>,
) -> Result<ObjectRef, AdapterError> {
    let name = name.ok_or_else(|| {
        AdapterError::new("playerHandleForProfile requires a player name")
    })?;
    let uuid = uuid.ok_or_else(|| {
        AdapterError::new("playerHandleForProfile requires a UUID string")
    })?;
    active_player_handle_for_profile(
        name,
        parse_uuid_string(uuid, "playerHandleForProfile")?,
    )
}

/// Returns the count of players whose lifecycle has reached this worker.
///
/// The dedicated host is the sole producer: it copies its connected roster,
/// queues joins and disconnects, and this map changes only when those
/// callbacks are processed. The count intentionally reports that reconciled
/// snapshot rather than reading a server registry from JNI. It therefore has
/// no world, connection, or guard behind it.
fn active_player_count() -> Result<jint, AdapterError> {
    ACTIVE_PLAYER_HANDLES.with(|slot| {
        let active = slot.borrow();
        let active = active.as_ref().ok_or_else(|| {
            AdapterError::new("activePlayerCount requires the adapter worker thread")
        })?;
        jint::try_from(active.len())
            .map_err(|_| AdapterError::new("activePlayerCount exceeds Java int range"))
    })
}

/// Removes and invalidates the worker-owned handle for one disconnected
/// player.
fn release_active_player_handle(
    identity: &LifecycleIdentity,
    player: &PlayerIdentity,
) -> Option<ObjectRef> {
    let handle = ACTIVE_PLAYER_HANDLES.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .and_then(|active| active.remove(player))
    })?;
    RESIDENT_OBJECT_HANDLES.with(|slot| {
        if let Some(registry) = slot.borrow_mut().as_mut() {
            let released = registry.release_matching(|kind, payload| {
                matches!(
                    (kind, payload),
                    (
                        ObjectKind::Player,
                        ResidentObject::Player { owner, player: resident }
                    ) if owner == identity && resident == player
                )
            });
            debug_assert_eq!(released, 1, "active player handle must have one registry entry");
        }
    });
    Some(handle)
}

fn resolve_resident_block_handle(
    bits: i64,
    operation: &str,
) -> Result<(i32, i32, i32), AdapterError> {
    let handle = ObjectRef::from_bits(bits, ObjectKind::Block);
    RESIDENT_OBJECT_HANDLES.with(|slot| {
        let handles = slot.borrow();
        let handles = handles.as_ref().ok_or_else(|| {
            AdapterError::new(format!("{operation} requires the adapter worker thread"))
        })?;
        handles
            .resolve(handle, ObjectKind::Block)
            .and_then(|object| match object {
                ResidentObject::Block { position, .. } => Ok(*position),
                ResidentObject::Player { .. } => Err(ResolveError::KindMismatch {
                    expected: ObjectKind::Block,
                    actual: ObjectKind::Player,
                }),
            })
            .map_err(|error| AdapterError::new(format!("{operation}: {error}")))
    })
}

/// Returns one copied coordinate from a generation-checked block handle.
///
/// The adapter mints a handle only while delivering a host-confirmed resident
/// block-state change. Its position remains a worker-owned value, so these
/// accessors need neither a world-port request nor an ECS pointer. Each named
/// accessor resolves the generation before returning a coordinate, making a
/// released or wrong-kind `long` fail loudly instead of reading a replacement.
fn resident_block_handle_coordinate(
    bits: i64,
    coordinate: usize,
    operation: &str,
) -> Result<jint, AdapterError> {
    let position = resolve_resident_block_handle(bits, operation)?;
    Ok([position.0, position.1, position.2][coordinate])
}

/// Resolves a block handle before requesting its current state from the host.
///
/// The coordinate lookup happens entirely in the worker-owned registry, then
/// the value query uses the same bounded port as a coordinate query. A stale
/// or wrong-kind handle therefore fails before it can reach the host, and the
/// host still owns the distinction between a resident air block and an absent
/// column.
pub(crate) fn resident_block_handle_state_id(bits: i64) -> Result<jint, AdapterError> {
    let (x, y, z) = resolve_resident_block_handle(bits, "blockHandleStateId")?;
    let port = CALLBACK_PORT.with(|slot| slot.borrow().clone())
        .ok_or_else(|| AdapterError::new("blockHandleStateId requires the adapter worker thread"))?;
    let state = port.request(BlockStateQuery { x, y, z })
        .map_err(|error| AdapterError::new(format!("blockHandleStateId: {error}")))?
        .map_err(|error| AdapterError::new(format!("blockHandleStateId({x},{y},{z}): {error}")))?;
    jint::try_from(state)
        .map_err(|_| AdapterError::new("blockHandleStateId exceeds Java int range"))
}

fn resolve_resident_player_handle(
    bits: i64,
    operation: &str,
) -> Result<PlayerIdentity, AdapterError> {
    let handle = ObjectRef::from_bits(bits, ObjectKind::Player);
    RESIDENT_OBJECT_HANDLES.with(|slot| {
        let handles = slot.borrow();
        let handles = handles.as_ref().ok_or_else(|| {
            AdapterError::new(format!("{operation} requires the adapter worker thread"))
        })?;
        handles
            .resolve(handle, ObjectKind::Player)
            .and_then(|object| match object {
                ResidentObject::Player { player, .. } => Ok(player.clone()),
                ResidentObject::Block { .. } => Err(ResolveError::KindMismatch {
                    expected: ObjectKind::Player,
                    actual: ObjectKind::Block,
                }),
            })
            .map_err(|error| AdapterError::new(format!("{operation}: {error}")))
    })
}

fn resolve_resident_player_handle_name(bits: i64) -> Result<String, AdapterError> {
    resolve_resident_player_handle(bits, "playerHandleName").map(|player| player.name().to_owned())
}

/// Returns a canonical UUID string copied from a generation-checked player handle.
///
/// The UUID never crosses the bridge as a server object: it is the sixteen
/// profile bytes copied by the dedicated host when it observes its roster.
fn resolve_resident_player_handle_uuid(bits: i64) -> Result<String, AdapterError> {
    Ok(canonical_uuid_string(
        resolve_resident_player_handle(bits, "playerHandleUuid")?.uuid(),
    ))
}

/// Reports whether a live player handle's copied profile is in the worker's
/// reconciled lifecycle map.
///
/// This resolves the supplied generation before consulting the map, so an old
/// `long` cannot become active again when a slot is reused. The answer is a
/// worker snapshot: the dedicated host is the sole producer of joins and
/// disconnects, and no server registry, connection, ECS value, or guard is
/// read from JNI.
fn resolve_resident_player_handle_is_active(bits: i64) -> Result<bool, AdapterError> {
    let player = resolve_resident_player_handle(bits, "playerHandleIsActive")?;
    ACTIVE_PLAYER_HANDLES.with(|slot| {
        let active = slot.borrow();
        let active = active.as_ref().ok_or_else(|| {
            AdapterError::new("playerHandleIsActive requires the adapter worker thread")
        })?;
        Ok(active.contains_key(&player))
    })
}

fn release_resident_handles(identity: &LifecycleIdentity) -> usize {
    RESIDENT_OBJECT_HANDLES.with(|slot| {
        slot.borrow_mut().as_mut().map_or(0, |handles| {
            handles.release_matching(|kind, payload| {
                match payload {
                    ResidentObject::Block { owner, .. } => {
                        kind == ObjectKind::Block && owner == identity
                    }
                    ResidentObject::Player { owner, .. } => {
                        kind == ObjectKind::Player && owner == identity
                    }
                }
            })
        })
    })
}

fn clear_resident_handles() -> usize {
    RESIDENT_OBJECT_HANDLES.with(|slot| {
        slot.borrow_mut().as_mut().map_or(0, |handles| handles.clear())
    })
}

#[derive(Clone, Copy)]
enum LifecycleIdentityField {
    Name,
    Version,
    MainClass,
}

impl Drop for LifecycleIdentityGuard {
    fn drop(&mut self) {
        LIFECYCLE_IDENTITY.with(|identities| {
            identities
                .borrow_mut()
                .pop()
                .expect("a lifecycle identity guard must own one worker-local identity");
        });
    }
}

/// Runs one constructor or lifecycle callback with its descriptor identity.
///
/// The caller is the only code that can install this context, and it is
/// crate-private so it remains coupled to the retained worker-owned entries.
pub(crate) fn with_lifecycle_identity<T>(
    name: &str,
    version: &str,
    main_class: &str,
    operation: impl FnOnce() -> T,
) -> T {
    LIFECYCLE_IDENTITY.with(|identities| {
        identities
            .borrow_mut()
            .push(lifecycle_identity(name, version, main_class));
    });
    let _identity = LifecycleIdentityGuard;
    operation()
}

/// One host-to-JVM callback waiting on the dedicated adapter worker.
///
/// The single command channel and [`State::Running`] permit only one of these
/// at a time. A block-change callback therefore cannot be dispatched while a
/// native read or write callback is waiting for the host.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AdapterCommand {
    Tick(u64),
    PlayerJoined(PlayerIdentity),
    PlayerDisconnected(PlayerIdentity),
    BlockStateChanged {
        change: BlockStateWrite,
        player: Option<PlayerIdentity>,
    },
}

/// A completed worker transition. Payload-bearing events preserve the host's
/// original callback values so callers can match completion without guessing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdapterEvent {
    Ready,
    TickCompleted(u64),
    /// A host-confirmed player join was delivered to the worker callback.
    /// `handle` is opaque Java `long` data and remains valid until the matching
    /// disconnect transition completes.
    PlayerJoinedCompleted {
        player: PlayerIdentity,
        handle: i64,
    },
    /// A host-confirmed player disconnect was delivered and its handle was
    /// invalidated. `handle` is `None` only when a host reports a disconnect
    /// for a player this worker had not observed joining.
    PlayerDisconnectedCompleted {
        player: PlayerIdentity,
        handle: Option<i64>,
    },
    /// The adapter callback and every registered isolated listener were
    /// invoked on the one worker. Listener failures are reported here instead
    /// of making the adapter terminal.
    BlockStateChangedCompleted {
        change: BlockStateWrite,
        /// The value-only player identity supplied for the reported block replacement, if any.
        /// The listener sees it only through the worker-owned player handle.
        player: Option<PlayerIdentity>,
        listener_failures: Vec<ResidentBlockChangeListenerFailure>,
    },
}

#[derive(Debug)]
enum State {
    Loading(Instant),
    Idle,
    Running { command: AdapterCommand, started: Instant },
    Failed(AdapterError),
}

/// Nonblocking host endpoint for one explicitly configured Java adapter.
///
/// Drop disconnects the worker and port without joining an untrusted Java call.
/// Java code cannot be forcibly stopped safely in-process: a timed-out adapter
/// is terminal and may continue executing until the operator exits the process.
/// JNI permits only one JVM startup per process, including after a failed load.
#[derive(Debug)]
pub struct AdapterHost {
    commands: SyncSender<AdapterCommand>,
    events: Receiver<Result<AdapterEvent, AdapterError>>,
    servicer: PortServicer<BlockStateQuery, BlockStateAnswer>,
    block_write_servicer: PortServicer<BlockStateWrite, BlockStateWriteAnswer>,
    server_tick_servicer: PortServicer<(), ServerTickAnswer>,
    deadline: Duration,
    state: State,
}

impl AdapterHost {
    /// Spawns the JVM worker. Loading success or failure arrives through `poll`.
    /// The class name uses dotted Java binary-name spelling, such as `a.B$C`.
    pub fn start(
        config: JvmConfig,
        class: &str,
        deadline: Duration,
    ) -> Result<Self, AdapterError> {
        Self::start_with_setup(config, class, deadline, |_, _, _| Ok(()))
    }

    /// Spawns the JVM worker after running one bounded setup step on it.
    ///
    /// The setup runs after the JVM starts but before the adapter class loads.
    /// It receives no world state and must not invoke arbitrary operator code.
    /// Its [`NativeServerSurface`] token proves that the same worker owns
    /// native request ports with host-side producers; it is the only way a
    /// lifecycle host may claim those narrow capabilities. A successful result
    /// remains on that worker until the adapter stops, so setup can retain
    /// loader-owned JVM state without publishing it to the tick thread.
    pub fn start_with_setup<F, S>(
        config: JvmConfig,
        class: &str,
        deadline: Duration,
        setup: F,
    ) -> Result<Self, AdapterError>
    where
        F: for<'local> FnOnce(&JvmRuntime, &mut Env<'local>, NativeServerSurface)
                -> Result<S, String>
            + Send
            + 'static,
        S: 'static,
    {
        validate_class(class)?;
        if deadline.is_zero() {
            return Err(AdapterError::new("adapter deadline must be positive"));
        }
        let class = class.to_owned();
        Self::spawn(deadline, move |commands, events, port, block_write_port, server_tick_port| {
            let result = run_java(
                config,
                &class,
                commands,
                &events,
                port,
                block_write_port,
                server_tick_port,
                setup,
            );
            if let Err(error) = result {
                let _ = events.send(Err(error));
            }
        })
    }

    fn spawn(
        deadline: Duration,
        run: impl FnOnce(Receiver<AdapterCommand>, Events, BlockPort, BlockWritePort, TickPort)
            + Send
            + 'static,
    ) -> Result<Self, AdapterError> {
        let (commands, receiver) = sync_channel(1);
        let (sender, events) = sync_channel(1);
        let (port, servicer) = channel(deadline);
        let (block_write_port, block_write_servicer) = channel(deadline);
        let (server_tick_port, server_tick_servicer) = channel(deadline);
        std::thread::Builder::new()
            .name("lodestone-java-adapter".to_owned())
            .spawn(move || run(receiver, sender, port, block_write_port, server_tick_port))
            .map_err(|error| AdapterError::new(format!("adapter worker startup: {error}")))?;
        Ok(Self {
            commands,
            events,
            servicer,
            block_write_servicer,
            server_tick_servicer,
            deadline,
            state: State::Loading(Instant::now()),
        })
    }

    /// True after the host polls readiness or the preceding tick's completion.
    pub fn is_idle(&self) -> bool {
        matches!(self.state, State::Idle)
    }

    /// Queues exactly one tick without blocking. Busy dispatch is an error;
    /// callbacks are never silently dropped or accumulated into a backlog.
    pub fn dispatch_tick(&mut self, tick: u64) -> Result<(), AdapterError> {
        if tick > i64::MAX as u64 {
            return Err(AdapterError::new("adapter tick exceeds Java long range"));
        }
        self.dispatch_command(AdapterCommand::Tick(tick), "tick")
    }

    /// Queues a host-confirmed player join for the adapter worker.
    ///
    /// The worker mints the generation-checked handle and invokes the adapter's
    /// `onPlayerJoined(long)` callback outside every server-world guard. The
    /// identity is copied by value; no ECS entity, connection, or lock guard
    /// crosses this boundary. A repeated join for the same profile is
    /// idempotent while that profile remains active.
    pub fn dispatch_player_joined(&mut self, player: PlayerIdentity) -> Result<(), AdapterError> {
        self.dispatch_command(AdapterCommand::PlayerJoined(player), "player join")
    }

    /// Queues a host-confirmed player disconnect for the adapter worker.
    ///
    /// The worker invokes `onPlayerDisconnected(long)` while the old handle is
    /// still resolvable, then advances its generation and releases the slot.
    /// A disconnect for an unobserved profile completes with no handle and does
    /// not mint one just to tear it down.
    pub fn dispatch_player_disconnected(
        &mut self,
        player: PlayerIdentity,
    ) -> Result<(), AdapterError> {
        self.dispatch_command(
            AdapterCommand::PlayerDisconnected(player),
            "player disconnect",
        )
    }

    /// Queues one host-confirmed block-state change for the adapter worker.
    ///
    /// Call this only after the host has applied a successful resident-block
    /// mutation. The callback runs on the dedicated JVM worker, never on the
    /// tick thread, and has no direct route to an ECS handle. A busy worker is
    /// rejected rather than accumulating events or calling Java while a
    /// native world request is unresolved.
    ///
    /// This is a narrow bridge callback, not a Bukkit or Paper event. It calls
    /// the explicit adapter method `onBlockStateChanged(int, int, int, int)`;
    /// no listener registry, cancellation, plugin instance, or Paper event
    /// type exists yet. Use [`Self::dispatch_block_state_changed_for_player`]
    /// when the host has a value-only player identity for the change.
    pub fn dispatch_block_state_changed(
        &mut self,
        change: BlockStateWrite,
    ) -> Result<(), AdapterError> {
        self.dispatch_block_state_changed_for_player(change, None)
    }

    /// Queues one host-confirmed resident block-state change with its value-only
    /// player identity, if a player caused the change.
    ///
    /// The existing listener callback remains the boundary: a listener calls
    /// `currentPlayerHandle()` while it runs, and may retain the returned bits
    /// for later `playerHandleName(long)` resolution. The identity is copied
    /// into the worker-owned generational registry; no ECS entity or server
    /// pointer is sent to Java.
    pub fn dispatch_block_state_changed_for_player(
        &mut self,
        change: BlockStateWrite,
        player: Option<PlayerIdentity>,
    ) -> Result<(), AdapterError> {
        if i32::try_from(change.state_id).is_err() {
            return Err(AdapterError::new(
                "block-state callback state id exceeds Java int range",
            ));
        }
        self.dispatch_command(
            AdapterCommand::BlockStateChanged { change, player },
            "block-state callback",
        )
    }

    fn dispatch_command(
        &mut self,
        command: AdapterCommand,
        operation: &str,
    ) -> Result<(), AdapterError> {
        if !self.is_idle() {
            return Err(AdapterError::new("adapter is not idle; poll its pending operation"));
        }
        self.commands.try_send(command.clone())
            .map_err(|error| AdapterError::new(format!("adapter {operation} dispatch: {error}")))?;
        self.state = State::Running { command, started: Instant::now() };
        Ok(())
    }

    /// Answers at most `max` queued block queries on the caller's thread.
    /// The closure should use the public host block API and report unavailable
    /// positions explicitly. It is never invoked on the JVM worker.
    pub fn service_pending(
        &self,
        max: usize,
        answer: impl FnMut(BlockStateQuery) -> BlockStateAnswer,
    ) -> usize {
        if matches!(self.state, State::Failed(_)) {
            return 0;
        }
        self.servicer.service_all_pending(max, answer)
    }

    /// Applies at most `max` native block-state writes on the caller's thread.
    ///
    /// The closure must preserve the host's resident-only mutation contract:
    /// it must reject an absent column rather than generate one on behalf of a
    /// Java callback. It is never invoked on the JVM worker.
    pub fn service_pending_block_writes(
        &self,
        max: usize,
        answer: impl FnMut(BlockStateWrite) -> BlockStateWriteAnswer,
    ) -> usize {
        if matches!(self.state, State::Failed(_)) {
            return 0;
        }
        self.block_write_servicer.service_all_pending(max, answer)
    }

    /// Answers at most `max` queued server-tick reads on the caller's thread.
    ///
    /// The closure must read the host's live tick witness. An inactive server
    /// is an error, not a made-up zero, because zero is a valid tick count for
    /// a future host whose lifecycle has not completed boot yet.
    pub fn service_pending_server_tick(
        &self,
        max: usize,
        mut answer: impl FnMut() -> ServerTickAnswer,
    ) -> usize {
        if matches!(self.state, State::Failed(_)) {
            return 0;
        }
        self.server_tick_servicer.service_all_pending(max, |_| answer())
    }

    /// Polls completion without waiting. Startup/callback deadlines are terminal;
    /// poll regularly even when no world query is pending. Java exceptions name
    /// the failing class/method and preserve the Java exception's description.
    pub fn poll(&mut self) -> Result<Option<AdapterEvent>, AdapterError> {
        if let State::Failed(error) = &self.state {
            return Err(error.clone());
        }
        let started = match &self.state {
            State::Loading(started) | State::Running { started, .. } => Some(*started),
            _ => None,
        };
        if started.is_some_and(|started| started.elapsed() >= self.deadline) {
            let error = AdapterError::new(format!("adapter operation exceeded {:?}", self.deadline));
            self.state = State::Failed(error.clone());
            return Err(error);
        }
        let result = match self.events.try_recv() {
            Ok(Ok(event)) => match (&self.state, &event) {
                (State::Loading(_), AdapterEvent::Ready) => Ok(Some(event)),
                (
                    State::Running { command: AdapterCommand::Tick(tick), .. },
                    AdapterEvent::TickCompleted(done),
                ) if *tick == *done => {
                    Ok(Some(event))
                }
                (
                    State::Running {
                        command: AdapterCommand::PlayerJoined(expected),
                        ..
                    },
                    AdapterEvent::PlayerJoinedCompleted { player, .. },
                ) if expected == player => Ok(Some(event)),
                (
                    State::Running {
                        command: AdapterCommand::PlayerDisconnected(expected),
                        ..
                    },
                    AdapterEvent::PlayerDisconnectedCompleted { player, .. },
                ) if expected == player => Ok(Some(event)),
                (
                    State::Running {
                        command: AdapterCommand::BlockStateChanged {
                            change: expected,
                            player,
                        },
                        ..
                    },
                    AdapterEvent::BlockStateChangedCompleted {
                        change,
                        player: completed_player,
                        ..
                    },
                ) if *expected == *change && player.as_ref() == completed_player.as_ref() => {
                    Ok(Some(event))
                }
                _ => Err(AdapterError::new("adapter worker returned an unexpected completion")),
            },
            Ok(Err(error)) => Err(error),
            Err(TryRecvError::Disconnected) => Err(AdapterError::new("adapter worker disconnected")),
            Err(TryRecvError::Empty) => Ok(None),
        };
        match &result {
            Ok(Some(_)) => self.state = State::Idle,
            Err(error) => self.state = State::Failed(error.clone()),
            Ok(None) => {}
        }
        result
    }
}

/// A bounded, actionable adapter failure; never a default block-state result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterError(String);

impl AdapterError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AdapterError {}

fn validate_class(class: &str) -> Result<(), AdapterError> {
    let valid = class.split('.').all(|segment| {
        let mut chars = segment.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    });
    if valid {
        Ok(())
    } else {
        Err(AdapterError::new(format!("invalid adapter class {class:?}")))
    }
}

fn run_java<S>(
    config: JvmConfig,
    class_name: &str,
    commands: Receiver<AdapterCommand>,
    events: &Events,
    port: BlockPort,
    block_write_port: BlockWritePort,
    server_tick_port: TickPort,
    setup: impl for<'local> FnOnce(&JvmRuntime, &mut Env<'local>, NativeServerSurface)
        -> Result<S, String>,
) -> Result<(), AdapterError> {
    // Operator paths are supplied only to isolated loaders below. Putting
    // either the adapter or a Paper jar on the system loader would let its
    // parent-first lookup defeat shim-first resolution.
    let runtime = JvmRuntime::start(&JvmConfig::new())
        .map_err(|error| AdapterError::new(format!("adapter {class_name}: {error}")))?;
    runtime.with_attached_thread(|env| {
        let result = (|| {
            CALLBACK_PORT.with(|slot| *slot.borrow_mut() = Some(port.clone()));
            BLOCK_WRITE_PORT.with(|slot| *slot.borrow_mut() = Some(block_write_port.clone()));
            SERVER_TICK_PORT.with(|slot| *slot.borrow_mut() = Some(server_tick_port.clone()));
            RESIDENT_OBJECT_HANDLES.with(|slot| {
                *slot.borrow_mut() = Some(ObjectRegistry::with_capacity(
                    MAX_RESIDENT_OBJECT_HANDLES,
                ))
            });
            ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = Some(HashMap::new()));
            CURRENT_RESIDENT_BLOCK_HANDLE.with(|slot| *slot.borrow_mut() = None);
            CURRENT_RESIDENT_PLAYER_HANDLE.with(|slot| *slot.borrow_mut() = None);
            RESIDENT_BLOCK_CHANGE_SUBSCRIPTIONS
                .with(|slot| *slot.borrow_mut() = Some(Default::default()));
            let surface = NativeServerSurface::from_ports(
                port.clone(),
                block_write_port.clone(),
                server_tick_port.clone(),
            );
            let setup_state = setup(&runtime, env, surface).map_err(|error| {
                AdapterError::new(format!("adapter {class_name} setup: {error}"))
            })?;
            let class = runtime.load_isolated_class(env, &config, class_name)
                .map_err(|error| java_error(env, class_name, error))?;
            let adapter_identity = lifecycle_identity(class_name, "adapter", class_name);
            register_block_query(env, &class, "blockStateId", "(III)I")
                .map_err(|error| java_error(env, &format!("{class_name}.blockStateId(III)I"), error))?;
            env.get_static_method_id(&class, jni_str!("onTick"), jni_sig!("(J)V"))
                .map_err(|error| java_error(env, &format!("{class_name}.onTick(J)V"), error))?;
            env.get_static_method_id(&class, jni_str!("onBlockStateChanged"), jni_sig!("(IIII)V"))
                .map_err(|error| java_error(
                    env,
                    &format!("{class_name}.onBlockStateChanged(IIII)V"),
                    error,
                ))?;
            env.get_static_method_id(&class, jni_str!("onPlayerJoined"), jni_sig!("(J)V"))
                .map_err(|error| java_error(
                    env,
                    &format!("{class_name}.onPlayerJoined(J)V"),
                    error,
                ))?;
            env.get_static_method_id(
                &class,
                jni_str!("onPlayerDisconnected"),
                jni_sig!("(J)V"),
            )
            .map_err(|error| java_error(
                env,
                &format!("{class_name}.onPlayerDisconnected(J)V"),
                error,
            ))?;
            if events.send(Ok(AdapterEvent::Ready)).is_err() {
                return Ok(());
            }
            while let Ok(command) = commands.recv() {
                let completion = match command {
                    AdapterCommand::Tick(tick) => {
                        env.with_local_frame(16, |env| {
                            env.call_static_method(&class, jni_str!("onTick"), jni_sig!("(J)V"),
                                &[JValue::Long(tick as i64)])
                                .map(|_| ())
                                .map_err(|error| java_error(
                                    env,
                                    &format!("{class_name}.onTick(J)V"),
                                    error,
                                ))
                        })?;
                        AdapterEvent::TickCompleted(tick)
                    }
                    AdapterCommand::PlayerJoined(player) => {
                        let handle = active_player_handle(&adapter_identity, &player)
                            .map_err(|error| AdapterError::new(error.to_string()))?;
                        env.with_local_frame(16, |env| {
                            env.call_static_method(
                                &class,
                                jni_str!("onPlayerJoined"),
                                jni_sig!("(J)V"),
                                &[JValue::Long(handle.to_bits())],
                            )
                            .map(|_| ())
                            .map_err(|error| java_error(
                                env,
                                &format!("{class_name}.onPlayerJoined(J)V"),
                                error,
                            ))
                        })?;
                        AdapterEvent::PlayerJoinedCompleted {
                            handle: handle.to_bits(),
                            player,
                        }
                    }
                    AdapterCommand::PlayerDisconnected(player) => {
                        let handle = active_player_handle_for(&player);
                        if let Some(handle) = handle {
                            // Keep the old slot live for the callback so a
                            // plugin may inspect the departing player. The
                            // release occurs immediately after this call,
                            // before completion is reported to the host.
                            env.with_local_frame(16, |env| {
                                env.call_static_method(
                                    &class,
                                    jni_str!("onPlayerDisconnected"),
                                    jni_sig!("(J)V"),
                                    &[JValue::Long(handle.to_bits())],
                                )
                                .map(|_| ())
                                .map_err(|error| java_error(
                                    env,
                                    &format!("{class_name}.onPlayerDisconnected(J)V"),
                                    error,
                                ))
                            })?;
                            // The callback above is intentionally the last
                            // place the handle can resolve. Release after it,
                            // so an old Java long fails as stale from the next
                            // command onward.
                            let released =
                                release_active_player_handle(&adapter_identity, &player);
                            debug_assert_eq!(released, Some(handle));
                        }
                        AdapterEvent::PlayerDisconnectedCompleted {
                            handle: handle.map(ObjectRef::to_bits),
                            player,
                        }
                    }
                    AdapterCommand::BlockStateChanged { change, player } => {
                        let state_id = i32::try_from(change.state_id).map_err(|_| {
                            AdapterError::new("block-state callback state id exceeds Java int range")
                        })?;
                        env.with_local_frame(16, |env| {
                            env.call_static_method(
                                &class,
                                jni_str!("onBlockStateChanged"),
                                jni_sig!("(IIII)V"),
                                &[
                                    JValue::Int(change.x),
                                    JValue::Int(change.y),
                                    JValue::Int(change.z),
                                    JValue::Int(state_id),
                                ],
                            )
                            .map(|_| ())
                            .map_err(|error| java_error(
                                env,
                                &format!("{class_name}.onBlockStateChanged(IIII)V"),
                                error,
                                ))
                        })?;
                        let listener_failures =
                            dispatch_resident_block_change(env, change, player.as_ref());
                        AdapterEvent::BlockStateChangedCompleted {
                            change,
                            player,
                            listener_failures,
                        }
                    }
                };
                if events.send(Ok(completion)).is_err() {
                    break;
                }
            }
            drop(setup_state);
            Ok(())
        })();
        clear_resident_handles();
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = None);
        CURRENT_RESIDENT_BLOCK_HANDLE.with(|slot| *slot.borrow_mut() = None);
        CURRENT_RESIDENT_PLAYER_HANDLE.with(|slot| *slot.borrow_mut() = None);
        CALLBACK_PORT.with(|slot| *slot.borrow_mut() = None);
        BLOCK_WRITE_PORT.with(|slot| *slot.borrow_mut() = None);
        SERVER_TICK_PORT.with(|slot| *slot.borrow_mut() = None);
        RESIDENT_BLOCK_CHANGE_SUBSCRIPTIONS.with(|slot| *slot.borrow_mut() = None);
        Ok(result)
    }).map_err(|error| AdapterError::new(error.to_string()))?
}

impl From<jni::errors::Error> for AdapterError {
    fn from(error: jni::errors::Error) -> Self {
        Self::new(error.to_string())
    }
}

fn java_error(env: &mut Env<'_>, operation: &str, error: impl fmt::Display) -> AdapterError {
    let description = env.exception_occurred().and_then(|exception| {
        env.exception_clear();
        let description = env.call_method(&exception, jni_str!("toString"),
            jni_sig!("()Ljava/lang/String;"), &[]).ok()?.l().ok()?;
        let description = env.cast_local::<JString>(description).ok()?;
        description.try_to_string(env).ok()
    });
    env.exception_clear();
    AdapterError::new(format!("adapter {operation}: {}", description.unwrap_or_else(|| error.to_string())))
}

#[allow(unsafe_code)]
pub(crate) fn register_block_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the static native accepts exactly three jint arguments and
    // returns jint. Callers first validate the supplied method name and
    // descriptor against that exact declaration. EnvUnowned contains every
    // Rust unwind at the FFI boundary.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(&name, &signature,
            native_block_state_id as *mut c_void);
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_server_tick_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the static native accepts no arguments and returns a jlong.
    // Callers validate the supplied declaration before registration. The
    // callback contains panics and errors at the JNI boundary.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(&name, &signature,
            native_server_tick_count as *mut c_void);
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_block_state_write(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the static native accepts four jint arguments and returns jint.
    // The isolated native surface validates this exact declaration before the
    // function pointer is installed. The callback contains every error at the
    // JNI boundary.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(&name, &signature,
            native_block_state_write as *mut c_void);
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_lifecycle_plugin_name_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native takes no arguments and returns a
    // Java string. Its worker-local context has no route to world state.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_lifecycle_plugin_name as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_lifecycle_plugin_version_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native takes no arguments and returns a
    // Java string. Its worker-local context has no route to world state.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_lifecycle_plugin_version as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_lifecycle_plugin_main_class_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native takes no arguments and returns a
    // Java string. Its worker-local context has no route to world state.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_lifecycle_plugin_main_class as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_lifecycle_plugin_descriptor_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native takes no arguments and returns the
    // isolated descriptor value. Its worker-local context has no route to
    // world state or a server facade.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_lifecycle_plugin_descriptor as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_resident_block_change_subscription(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native takes one isolated listener object
    // and returns void. The native callback retains it only on this adapter
    // worker, where the matching dispatch command is single-flight.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_subscribe_resident_block_changes as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_current_block_handle_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native accepts no arguments and returns a
    // jlong. The callback returns only an opaque generation-checked handle;
    // it never publishes a host pointer or an ECS value.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_current_block_handle as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_block_handle_position_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native accepts one jlong and returns a
    // Java string. Resolution returns copied coordinates from the worker-local
    // value registry, never a pointer into the server ECS.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_block_handle_position as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_block_handle_x_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    register_block_handle_coordinate_query(
        env,
        class,
        method_name,
        descriptor,
        native_block_handle_x,
    )
}

#[allow(unsafe_code)]
pub(crate) fn register_block_handle_y_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    register_block_handle_coordinate_query(
        env,
        class,
        method_name,
        descriptor,
        native_block_handle_y,
    )
}

#[allow(unsafe_code)]
pub(crate) fn register_block_handle_z_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    register_block_handle_coordinate_query(
        env,
        class,
        method_name,
        descriptor,
        native_block_handle_z,
    )
}

#[allow(unsafe_code)]
fn register_block_handle_coordinate_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
    function: extern "system" fn(EnvUnowned<'_>, JClass<'_>, jlong) -> jint,
) -> jni::errors::Result<()> {
    // SAFETY: each validated static native accepts one opaque jlong and
    // returns a copied coordinate from the worker-local value registry. It
    // never requests or publishes a server-owned pointer.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(&name, &signature, function as *mut c_void);
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_block_handle_state_id_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native accepts an opaque jlong and returns
    // a jint. Resolution occurs before the bounded world-port request, so no
    // ECS pointer, handle, or guard crosses JNI.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_block_handle_state_id as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_current_player_handle_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native accepts no arguments and returns a
    // jlong. The callback returns only an opaque generation-checked handle;
    // it never publishes a player pointer or an ECS value.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_current_player_handle as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_player_handle_name_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native accepts one jlong and returns a
    // Java string. Resolution copies the value-only player name from the
    // worker-local registry, never a pointer into the server ECS.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_player_handle_name as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_player_handle_uuid_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native accepts one jlong and returns a
    // canonical copied UUID string from the worker-local registry, never a
    // player pointer, connection, or ECS value.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_player_handle_uuid as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_player_handle_for_uuid_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native accepts one Java string and returns
    // an opaque jlong. Resolution reads only the worker-owned copied roster;
    // it never publishes a player pointer, connection, or ECS guard.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_player_handle_for_uuid as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_player_handle_for_name_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native accepts one Java string and returns
    // an opaque jlong. It searches only the worker-owned copied roster and
    // rejects ambiguous names rather than publishing an unstable object.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_player_handle_for_name as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_player_handle_for_name_ignoring_case_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native accepts one Java string and returns
    // an opaque jlong. It compares only copied ASCII profile names and rejects
    // collisions rather than publishing a nondeterministically selected handle.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_player_handle_for_name_ignoring_case as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_player_handle_for_name_prefix_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native accepts one Java string and returns
    // an opaque jlong. It searches only the worker-owned copied roster and
    // rejects an empty or ambiguous prefix before returning a handle.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_player_handle_for_name_prefix as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_player_handle_for_profile_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native accepts copied Java name and UUID
    // strings and returns one opaque jlong. The complete profile is looked up
    // only in the worker-local lifecycle map; no server object or guard is
    // reachable from the JNI call.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_player_handle_for_profile as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_active_player_handle_at_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native accepts one jint and returns one
    // opaque jlong. Selection reads only the worker-owned copied roster and
    // never publishes a player pointer, connection, ECS value, or guard.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_active_player_handle_at as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_player_handle_is_active_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native accepts one opaque jlong and
    // returns a copied boolean from the worker-local lifecycle map. The
    // generation check precedes the map lookup, so no server object, pointer,
    // or guard reaches Java.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_player_handle_is_active as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_active_player_count_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the validated static native accepts no arguments and returns a
    // jint copied from the worker-owned lifecycle map. It never reads a server
    // registry or a world value at the JNI boundary.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(
            &name,
            &signature,
            native_active_player_count as *mut c_void,
        );
        env.register_native_methods(class, &[method])
    }
}

extern "system" fn native_block_state_id<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    x: jint,
    y: jint,
    z: jint,
) -> jint {
    env.with_env(|_env| -> Result<jint, AdapterError> {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        let port = CALLBACK_PORT.with(|slot| slot.borrow().clone())
            .ok_or_else(|| AdapterError::new("blockStateId requires the adapter worker thread"))?;
        let state = port.request(BlockStateQuery { x, y, z })
            .map_err(|error| AdapterError::new(format!("blockStateId: {error}")))?
            .map_err(|error| AdapterError::new(format!("blockStateId({x},{y},{z}): {error}")))?;
        jint::try_from(state).map_err(|_| AdapterError::new("blockStateId exceeds Java int range"))
    }).resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_server_tick_count<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> jlong {
    env.with_env(|_env| -> Result<jlong, AdapterError> {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        let port = SERVER_TICK_PORT.with(|slot| slot.borrow().clone())
            .ok_or_else(|| AdapterError::new("serverTickCount requires the adapter worker thread"))?;
        let tick = port.request(())
            .map_err(|error| AdapterError::new(format!("serverTickCount: {error}")))?
            .map_err(|error| AdapterError::new(format!("serverTickCount: {error}")))?;
        jlong::try_from(tick)
            .map_err(|_| AdapterError::new("serverTickCount exceeds Java long range"))
    }).resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_block_state_write<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    x: jint,
    y: jint,
    z: jint,
    state_id: jint,
) -> jint {
    env.with_env(|_env| -> Result<jint, AdapterError> {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        let state_id = u32::try_from(state_id)
            .map_err(|_| AdapterError::new("setBlockStateId requires a non-negative state id"))?;
        let port = BLOCK_WRITE_PORT.with(|slot| slot.borrow().clone())
            .ok_or_else(|| AdapterError::new("setBlockStateId requires the adapter worker thread"))?;
        port.request(BlockStateWrite { x, y, z, state_id })
            .map_err(|error| AdapterError::new(format!("setBlockStateId: {error}")))?
            .map_err(|error| AdapterError::new(format!("setBlockStateId({x},{y},{z},{state_id}): {error}")))?;
        jint::try_from(state_id)
            .map_err(|_| AdapterError::new("setBlockStateId exceeds Java int range"))
    }).resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_lifecycle_plugin_name<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> jstring {
    env.with_env(|env| lifecycle_identity_string(env, LifecycleIdentityField::Name))
        .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_lifecycle_plugin_version<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> jstring {
    env.with_env(|env| lifecycle_identity_string(env, LifecycleIdentityField::Version))
        .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_lifecycle_plugin_main_class<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> jstring {
    env.with_env(|env| lifecycle_identity_string(env, LifecycleIdentityField::MainClass))
        .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_lifecycle_plugin_descriptor<'local>(
    mut env: EnvUnowned<'local>,
    class: JClass<'local>,
) -> jobject {
    env.with_env(|env| lifecycle_plugin_descriptor(env, &class))
        .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_subscribe_resident_block_changes<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    listener: JObject<'local>,
) {
    env.with_env(|env| subscribe_resident_block_changes(env, listener))
        .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_current_block_handle<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> jlong {
    env.with_env(|_env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        Ok::<_, AdapterError>(current_resident_block_handle()?.to_bits())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_block_handle_position<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    bits: jlong,
) -> jstring {
    env.with_env(|env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        let position = resolve_resident_block_handle(bits, "blockHandlePosition")?;
        env.new_string(format!("{},{},{}", position.0, position.1, position.2))
            .map(|value| value.into_raw())
            .map_err(|error| AdapterError::new(format!("blockHandlePosition: {error}")))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_block_handle_x<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    bits: jlong,
) -> jint {
    env.with_env(|_env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        resident_block_handle_coordinate(bits, 0, "blockHandleX")
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_block_handle_y<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    bits: jlong,
) -> jint {
    env.with_env(|_env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        resident_block_handle_coordinate(bits, 1, "blockHandleY")
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_block_handle_z<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    bits: jlong,
) -> jint {
    env.with_env(|_env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        resident_block_handle_coordinate(bits, 2, "blockHandleZ")
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_block_handle_state_id<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    bits: jlong,
) -> jint {
    env.with_env(|_env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        resident_block_handle_state_id(bits)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_current_player_handle<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> jlong {
    env.with_env(|_env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        Ok::<_, AdapterError>(current_resident_player_handle()?.to_bits())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_player_handle_name<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    bits: jlong,
) -> jstring {
    env.with_env(|env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        let name = resolve_resident_player_handle_name(bits)?;
        env.new_string(name)
            .map(|value| value.into_raw())
            .map_err(|error| AdapterError::new(format!("playerHandleName: {error}")))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_player_handle_uuid<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    bits: jlong,
) -> jstring {
    env.with_env(|env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        let uuid = resolve_resident_player_handle_uuid(bits)?;
        env.new_string(uuid)
            .map(|value| value.into_raw())
            .map_err(|error| AdapterError::new(format!("playerHandleUuid: {error}")))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_player_handle_for_uuid<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    uuid: JString<'local>,
) -> jlong {
    env.with_env(|env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        if uuid.is_null() {
            return Err(AdapterError::new(
                "playerHandleForUuid requires a UUID string",
            ));
        }
        let uuid = uuid
            .try_to_string(env)
            .map_err(|error| AdapterError::new(format!("playerHandleForUuid: {error}")))?;
        resolve_active_player_uuid(&uuid)
            .map(ObjectRef::to_bits)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_player_handle_for_name<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    name: JString<'local>,
) -> jlong {
    env.with_env(|env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        let name = if name.is_null() {
            None
        } else {
            Some(
                name.try_to_string(env)
                    .map_err(|error| AdapterError::new(format!("playerHandleForName: {error}")))?,
            )
        };
        resolve_active_player_name(name.as_deref()).map(ObjectRef::to_bits)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_player_handle_for_name_ignoring_case<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    name: JString<'local>,
) -> jlong {
    env.with_env(|env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        let name = if name.is_null() {
            None
        } else {
            Some(name.try_to_string(env).map_err(|error| {
                AdapterError::new(format!("playerHandleForNameIgnoringCase: {error}"))
            })?)
        };
        resolve_active_player_name_ignoring_case(name.as_deref()).map(ObjectRef::to_bits)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_player_handle_for_name_prefix<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    prefix: JString<'local>,
) -> jlong {
    env.with_env(|env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        let prefix = if prefix.is_null() {
            None
        } else {
            Some(prefix.try_to_string(env).map_err(|error| {
                AdapterError::new(format!("playerHandleForNamePrefix: {error}"))
            })?)
        };
        resolve_active_player_name_prefix(prefix.as_deref()).map(ObjectRef::to_bits)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_player_handle_for_profile<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    name: JString<'local>,
    uuid: JString<'local>,
) -> jlong {
    env.with_env(|env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        let name = if name.is_null() {
            None
        } else {
            Some(name.try_to_string(env).map_err(|error| {
                AdapterError::new(format!("playerHandleForProfile: {error}"))
            })?)
        };
        let uuid = if uuid.is_null() {
            None
        } else {
            Some(uuid.try_to_string(env).map_err(|error| {
                AdapterError::new(format!("playerHandleForProfile: {error}"))
            })?)
        };
        resolve_active_player_profile(name.as_deref(), uuid.as_deref()).map(ObjectRef::to_bits)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_active_player_handle_at<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    index: jint,
) -> jlong {
    env.with_env(|_env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        active_player_handle_at(index).map(ObjectRef::to_bits)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_player_handle_is_active<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    bits: jlong,
) -> jboolean {
    env.with_env(|_env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        Ok::<_, AdapterError>(jboolean::from(resolve_resident_player_handle_is_active(bits)?))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_active_player_count<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> jint {
    env.with_env(|_env| {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        active_player_count()
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

fn subscribe_resident_block_changes(
    env: &mut Env<'_>,
    listener: JObject<'_>,
) -> Result<(), AdapterError> {
    let _depth = CallbackDepthGuard::enter()
        .map_err(|error| AdapterError::new(error.to_string()))?;
    if listener.is_null() {
        return Err(AdapterError::new(
            "subscribeResidentBlockStateChanges requires a listener",
        ));
    }
    let identity = active_subscription_identity()?;
    let listener = env
        .new_global_ref(listener)
        .map_err(|error| AdapterError::new(format!("resident block listener subscription: {error}")))?;
    RESIDENT_BLOCK_CHANGE_SUBSCRIPTIONS.with(|slot| {
        let mut subscriptions = slot.borrow_mut();
        let subscriptions = subscriptions.as_mut().ok_or_else(|| {
            AdapterError::new("subscribeResidentBlockStateChanges requires the adapter worker thread")
        })?;
        subscriptions.register(identity, listener)?;
        Ok(())
    })
}

fn active_subscription_identity() -> Result<LifecycleIdentity, AdapterError> {
    LIFECYCLE_IDENTITY.with(|identities| identities.borrow().last().cloned())
        .ok_or_else(|| AdapterError::new(
            "subscribeResidentBlockStateChanges requires an active retained-entry lifecycle call",
        ))
}

fn lifecycle_identity(name: &str, version: &str, main_class: &str) -> LifecycleIdentity {
    LifecycleIdentity {
        name: name.to_owned(),
        version: version.to_owned(),
        main_class: main_class.to_owned(),
    }
}

/// Marks registrations made by one entry active after its enable callback
/// succeeds. The lifecycle owner runs this only on the adapter worker.
pub(crate) fn activate_resident_block_change_subscriptions(
    name: &str,
    version: &str,
    main_class: &str,
) -> usize {
    let identity = lifecycle_identity(name, version, main_class);
    RESIDENT_BLOCK_CHANGE_SUBSCRIPTIONS.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .map_or(0, |subscriptions| subscriptions.activate(&identity))
    })
}

/// Drops all registrations owned by one entry after construction/enable
/// failure or after its disable callback. Dropping the JNI globals here keeps
/// cleanup on the attached adapter worker rather than an arbitrary thread.
pub(crate) fn clear_resident_block_change_subscriptions(
    name: &str,
    version: &str,
    main_class: &str,
) -> usize {
    let identity = lifecycle_identity(name, version, main_class);
    release_resident_handles(&identity);
    RESIDENT_BLOCK_CHANGE_SUBSCRIPTIONS.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .map_or(0, |subscriptions| subscriptions.clear(&identity))
    })
}

fn dispatch_resident_block_change(
    env: &mut Env<'_>,
    change: BlockStateWrite,
    player: Option<&PlayerIdentity>,
) -> Vec<ResidentBlockChangeListenerFailure> {
    let state_id = i32::try_from(change.state_id)
        .expect("block-state callback range was checked before worker dispatch");
    let mut failures = Vec::new();
    let mut listeners = Vec::new();
    RESIDENT_BLOCK_CHANGE_SUBSCRIPTIONS.with(|slot| {
        let subscriptions = slot.borrow();
        let Some(subscriptions) = subscriptions.as_ref() else {
            return;
        };
        for (registration, identity, listener) in subscriptions.active_entries() {
            match env.new_local_ref(listener.as_obj()) {
                Ok(listener) => {
                    let handle = match resident_block_handle(identity, change) {
                        Ok(handle) => Some(handle),
                        Err(error) => {
                            failures.push(ResidentBlockChangeListenerFailure {
                                registration,
                                plugin_name: identity.name.clone(),
                                detail: error.to_string(),
                            });
                            None
                        }
                    };
                    let player_handle = match player {
                        Some(player) => match resident_player_handle(identity, player) {
                            Ok(handle) => Some(handle),
                            Err(error) => {
                                failures.push(ResidentBlockChangeListenerFailure {
                                    registration,
                                    plugin_name: identity.name.clone(),
                                    detail: error.to_string(),
                                });
                                None
                            }
                        },
                        None => None,
                    };
                    listeners.push((registration, identity.clone(), listener, handle, player_handle));
                }
                Err(error) => failures.push(ResidentBlockChangeListenerFailure {
                    registration,
                    plugin_name: identity.name.clone(),
                    detail: bound_listener_detail(format!(
                        "could not retain listener local reference: {error}"
                    )),
                }),
            }
        }
    });
    failures.extend(dispatch_isolated_listeners(
        listeners.into_iter().map(|(registration, identity, listener, handle, player_handle)| {
            (
                registration,
                identity.name.clone(),
                (identity, listener, handle, player_handle),
            )
        }),
        |(identity, listener, handle, player_handle)| {
            with_lifecycle_identity(
                &identity.name,
                &identity.version,
                &identity.main_class,
                || {
                    let _block_handle = ResidentBlockHandleGuard::enter(*handle);
                    let _player_handle = ResidentPlayerHandleGuard::enter(*player_handle);
                    let result = env.with_local_frame(16, |env| {
                        env.call_method(
                            listener,
                            jni_str!("onResidentBlockStateChanged"),
                            jni_sig!("(IIII)V"),
                            &[
                                JValue::Int(change.x),
                                JValue::Int(change.y),
                                JValue::Int(change.z),
                                JValue::Int(state_id),
                            ],
                        )
                        .map(|_| ())
                    });
                    result.map_err(|error| {
                        bound_listener_detail(
                            java_error(
                                env,
                                "resident block listener onResidentBlockStateChanged(IIII)V",
                                error,
                            )
                            .to_string(),
                        )
                    })
                },
            )
        },
    ));
    failures
}

const MAX_LISTENER_FAILURE_DETAIL: usize = 512;

fn bound_listener_detail(detail: String) -> String {
    if detail.len() <= MAX_LISTENER_FAILURE_DETAIL {
        return detail;
    }
    let mut end = MAX_LISTENER_FAILURE_DETAIL;
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &detail[..end])
}

fn lifecycle_identity_string(
    env: &mut Env<'_>,
    field: LifecycleIdentityField,
) -> Result<jstring, AdapterError> {
    let _depth = CallbackDepthGuard::enter()
        .map_err(|error| AdapterError::new(error.to_string()))?;
    let value = lifecycle_identity_value(field)?;
    env.new_string(value)
        .map(|value| value.into_raw())
        .map_err(|error| AdapterError::new(format!("plugin descriptor query: {error}")))
}

fn lifecycle_identity_value(field: LifecycleIdentityField) -> Result<String, AdapterError> {
    let identity = active_lifecycle_identity()?;
    Ok(match field {
        LifecycleIdentityField::Name => identity.name,
        LifecycleIdentityField::Version => identity.version,
        LifecycleIdentityField::MainClass => identity.main_class,
    })
}

fn lifecycle_plugin_descriptor(
    env: &mut Env<'_>,
    shim_class: &JClass<'_>,
) -> Result<jobject, AdapterError> {
    let _depth = CallbackDepthGuard::enter()
        .map_err(|error| AdapterError::new(error.to_string()))?;
    let identity = active_lifecycle_identity()?;
    let loader = env
        .call_method(
            shim_class,
            jni_str!("getClassLoader"),
            jni_sig!("()Ljava/lang/ClassLoader;"),
            &[],
        )
        .and_then(|value| value.l())
        .map_err(|error| AdapterError::new(format!("plugin descriptor query: {error}")))?;
    let binary_name = env
        .new_string(crate::native_surface::ISOLATED_PLUGIN_DESCRIPTOR_CLASS)
        .map_err(|error| AdapterError::new(format!("plugin descriptor query: {error}")))?;
    let descriptor_class = env
        .call_method(
            &loader,
            jni_str!("loadClass"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
            &[JValue::Object(&binary_name)],
        )
        .and_then(|value| value.l())
        .and_then(|class| env.cast_local::<JClass>(class))
        .map_err(|error| AdapterError::new(format!("plugin descriptor query: {error}")))?;
    let name = env
        .new_string(identity.name)
        .map_err(|error| AdapterError::new(format!("plugin descriptor query: {error}")))?;
    let version = env
        .new_string(identity.version)
        .map_err(|error| AdapterError::new(format!("plugin descriptor query: {error}")))?;
    let main_class = env
        .new_string(identity.main_class)
        .map_err(|error| AdapterError::new(format!("plugin descriptor query: {error}")))?;
    env.new_object(
        descriptor_class,
        jni_sig!("(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V"),
        &[
            JValue::Object(&name),
            JValue::Object(&version),
            JValue::Object(&main_class),
        ],
    )
    .map(JObject::into_raw)
    .map_err(|error| AdapterError::new(format!("plugin descriptor query: {error}")))
}

fn active_lifecycle_identity() -> Result<LifecycleIdentity, AdapterError> {
    LIFECYCLE_IDENTITY.with(|identities| identities.borrow().last().cloned())
        .ok_or_else(|| AdapterError::new(
            "plugin descriptor queries require an active retained-entry lifecycle call",
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_descriptor_identity_is_worker_scoped_and_restored() {
        assert_eq!(
            active_lifecycle_identity()
                .expect_err("out-of-scope descriptor query must fail")
                .to_string(),
            "plugin descriptor queries require an active retained-entry lifecycle call",
        );
        with_lifecycle_identity("outer", "one", "outer.Main", || {
            assert_eq!(
                lifecycle_identity_value(LifecycleIdentityField::MainClass),
                Ok("outer.Main".to_owned()),
                "the direct lifecycle query returns the validated main-class name",
            );
            let outer = active_lifecycle_identity().expect("outer identity");
            assert_eq!(outer.name, "outer");
            assert_eq!(outer.version, "one");
            assert_eq!(outer.main_class, "outer.Main");
            with_lifecycle_identity("inner", "two", "inner.Main", || {
                assert_eq!(
                    lifecycle_identity_value(LifecycleIdentityField::MainClass),
                    Ok("inner.Main".to_owned()),
                    "nested lifecycle identity must take precedence",
                );
                let inner = active_lifecycle_identity().expect("inner identity");
                assert_eq!(inner.name, "inner");
                assert_eq!(inner.version, "two");
                assert_eq!(inner.main_class, "inner.Main");
            });
            assert_eq!(
                active_lifecycle_identity().expect("restored outer identity").name,
                "outer",
            );
            assert_eq!(
                active_lifecycle_identity()
                    .expect("restored outer identity")
                    .main_class,
                "outer.Main",
            );
            assert_eq!(
                lifecycle_identity_value(LifecycleIdentityField::MainClass),
                Ok("outer.Main".to_owned()),
                "dropping the nested identity restores the direct query",
            );
        });
        assert_eq!(
            lifecycle_identity_value(LifecycleIdentityField::MainClass)
                .expect_err("direct query must fail outside the lifecycle scope")
                .to_string(),
            "plugin descriptor queries require an active retained-entry lifecycle call",
        );
        assert_eq!(
            active_lifecycle_identity()
                .expect_err("descriptor context must be removed after callback")
                .to_string(),
            "plugin descriptor queries require an active retained-entry lifecycle call",
        );
    }

    fn await_event(host: &mut AdapterHost) -> AdapterEvent {
        let limit = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(event) = host.poll().expect("worker event") {
                return event;
            }
            assert!(Instant::now() < limit, "worker did not report an event");
            std::thread::yield_now();
        }
    }

    #[test]
    fn callback_queries_run_on_host_and_ticks_do_not_overlap() {
        let host_thread = std::thread::current().id();
        let mut host = AdapterHost::spawn(Duration::from_secs(2), move |commands, events, port, _, _| {
            assert_ne!(std::thread::current().id(), host_thread);
            events.send(Ok(AdapterEvent::Ready)).unwrap();
            for command in commands {
                let AdapterCommand::Tick(tick) = command else {
                    panic!("fixture expected only a tick callback");
                };
                let result = port.request(BlockStateQuery { x: 11, y: 7, z: -3 }).unwrap();
                assert_eq!(result, Ok(422));
                events.send(Ok(AdapterEvent::TickCompleted(tick))).unwrap();
            }
        }).unwrap();
        assert!(host.dispatch_tick(37).is_err(), "loading is not ready");
        assert_eq!(await_event(&mut host), AdapterEvent::Ready);
        assert!(host.dispatch_tick(u64::MAX).is_err(), "do not truncate Java long");
        host.dispatch_tick(37).unwrap();
        assert!(host.dispatch_tick(38).is_err(), "no second tick while one is pending");
        let limit = Instant::now() + Duration::from_secs(2);
        while host.service_pending(1, |query| {
            assert_eq!(std::thread::current().id(), host_thread);
            assert_eq!(query, BlockStateQuery { x: 11, y: 7, z: -3 });
            Ok((query.x * 31 + query.y * 7 - query.z * 5 + 17) as u32)
        }) == 0 {
            assert!(Instant::now() < limit, "query did not reach the host");
            std::thread::yield_now();
        }
        assert_eq!(await_event(&mut host), AdapterEvent::TickCompleted(37));
        assert!(host.is_idle());
    }

    #[test]
    fn block_change_callbacks_are_bounded_and_preserve_the_applied_value() {
        let host_thread = std::thread::current().id();
        let expected = BlockStateWrite {
            x: -17,
            y: 64,
            z: 33,
            state_id: 1234,
        };
        let mut host = AdapterHost::spawn(Duration::from_secs(2), move |commands, events, _, _, _| {
            assert_ne!(std::thread::current().id(), host_thread);
            events.send(Ok(AdapterEvent::Ready)).unwrap();
            let command = commands.recv().expect("one host callback");
            assert_eq!(
                command,
                AdapterCommand::BlockStateChanged {
                    change: expected,
                    player: None,
                },
            );
            events.send(Ok(AdapterEvent::BlockStateChangedCompleted {
                change: expected,
                player: None,
                listener_failures: Vec::new(),
            })).unwrap();
        }).unwrap();
        assert_eq!(await_event(&mut host), AdapterEvent::Ready);
        host.dispatch_block_state_changed(expected).unwrap();
        assert!(host.dispatch_tick(37).is_err(), "no callback backlog is permitted");
        assert_eq!(
            await_event(&mut host),
            AdapterEvent::BlockStateChangedCompleted {
                change: expected,
                player: None,
                listener_failures: Vec::new(),
            },
        );
        assert!(host.is_idle());
        let oversized = BlockStateWrite { state_id: i32::MAX as u32 + 1, ..expected };
        let error = host
            .dispatch_block_state_changed(oversized)
            .expect_err("the Java callback cannot represent this native state id");
        assert!(error.to_string().contains("Java int range"), "{error}");
    }

    #[test]
    fn player_block_change_dispatch_preserves_the_value_identity() {
        let player = PlayerIdentity::new([9; 16], "Alice");
        let expected = BlockStateWrite {
            x: 2,
            y: 64,
            z: -4,
            state_id: 17,
        };
        let player_for_worker = player.clone();
        let mut host = AdapterHost::spawn(
            Duration::from_secs(2),
            move |commands, events, _, _, _| {
                events.send(Ok(AdapterEvent::Ready)).unwrap();
                assert_eq!(
                    commands.recv().expect("one player callback"),
                    AdapterCommand::BlockStateChanged {
                        change: expected,
                        player: Some(player_for_worker.clone()),
                    },
                );
                events
                    .send(Ok(AdapterEvent::BlockStateChangedCompleted {
                        change: expected,
                        player: Some(player_for_worker),
                        listener_failures: Vec::new(),
                    }))
                    .unwrap();
            },
        )
        .unwrap();
        assert_eq!(await_event(&mut host), AdapterEvent::Ready);
        host.dispatch_block_state_changed_for_player(expected, Some(player.clone()))
            .expect("player identity fits the callback");
        assert_eq!(
            await_event(&mut host),
            AdapterEvent::BlockStateChangedCompleted {
                change: expected,
                player: Some(player),
                listener_failures: Vec::new(),
            },
        );
    }

    #[test]
    fn player_lifecycle_dispatch_preserves_order_and_identity() {
        let player = PlayerIdentity::new([4; 16], "Alice");
        let joined = player.clone();
        let disconnected = player.clone();
        let mut host = AdapterHost::spawn(
            Duration::from_secs(2),
            move |commands, events, _, _, _| {
                events.send(Ok(AdapterEvent::Ready)).unwrap();
                assert_eq!(
                    commands.recv().expect("join transition"),
                    AdapterCommand::PlayerJoined(joined.clone()),
                );
                events
                    .send(Ok(AdapterEvent::PlayerJoinedCompleted {
                        player: joined,
                        handle: 4,
                    }))
                    .unwrap();
                assert_eq!(
                    commands.recv().expect("disconnect transition"),
                    AdapterCommand::PlayerDisconnected(disconnected.clone()),
                );
                events
                    .send(Ok(AdapterEvent::PlayerDisconnectedCompleted {
                        player: disconnected,
                        handle: Some(4),
                    }))
                    .unwrap();
            },
        )
        .unwrap();
        assert_eq!(await_event(&mut host), AdapterEvent::Ready);
        host.dispatch_player_joined(player.clone())
            .expect("join dispatch");
        assert_eq!(
            await_event(&mut host),
            AdapterEvent::PlayerJoinedCompleted {
                player: player.clone(),
                handle: 4,
            },
        );
        host.dispatch_player_disconnected(player.clone())
            .expect("disconnect dispatch");
        assert_eq!(
            await_event(&mut host),
            AdapterEvent::PlayerDisconnectedCompleted {
                player,
                handle: Some(4),
            },
        );
    }

    #[test]
    fn active_player_handle_release_advances_generation_and_bounds_cleanup() {
        let identity = lifecycle_identity("adapter", "adapter", "fixture.Adapter");
        let player = PlayerIdentity::new([5; 16], "Alice");
        RESIDENT_OBJECT_HANDLES.with(|slot| {
            *slot.borrow_mut() = Some(ObjectRegistry::with_capacity(1));
        });
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = Some(HashMap::new()));
        let first = active_player_handle(&identity, &player).expect("active player handle");
        assert_eq!(active_player_handle_for(&player), Some(first));
        assert_eq!(
            resolve_active_player_uuid("05050505-0505-0505-0505-050505050505"),
            Ok(first),
            "the worker reverse resolver must return the generation-matched handle",
        );
        assert_eq!(
            resolve_active_player_name(Some("Alice")),
            Ok(first),
            "the name resolver must return the same worker-owned live handle",
        );
        assert_eq!(
            resolve_active_player_name_ignoring_case(Some("aLiCe")),
            Ok(first),
            "case-insensitive lookup must still return the live generation",
        );
        assert_eq!(
            resolve_active_player_profile(
                Some("Alice"),
                Some("05050505-0505-0505-0505-050505050505"),
            ),
            Ok(first),
            "the complete-profile resolver must return the same worker-owned live handle",
        );
        assert_eq!(
            resolve_active_player_profile(None, Some("05050505-0505-0505-0505-050505050505")),
            Err(AdapterError::new(
                "playerHandleForProfile requires a player name",
            )),
            "a null Java name must fail before a roster lookup",
        );
        assert_eq!(
            resolve_active_player_profile(Some("Alice"), None),
            Err(AdapterError::new(
                "playerHandleForProfile requires a UUID string",
            )),
            "a null Java UUID must fail before a roster lookup",
        );
        assert_eq!(
            resolve_active_player_profile(Some("Alice"), Some("not-a-uuid")),
            Err(AdapterError::new(
                "playerHandleForProfile: invalid UUID \"not-a-uuid\" (expected 36-character form)",
            )),
            "a malformed profile UUID must fail before a roster lookup",
        );
        assert_eq!(
            resolve_active_player_name_ignoring_case(None),
            Err(AdapterError::new(
                "playerHandleForNameIgnoringCase requires a player name",
            )),
            "a null Java string must fail before a roster lookup",
        );
        assert_eq!(
            resolve_active_player_name_ignoring_case(Some("Alicé")),
            Err(AdapterError::new(
                "playerHandleForNameIgnoringCase: invalid non-ASCII player name \"Alicé\"",
            )),
            "non-ASCII input must not acquire locale-dependent matching semantics",
        );
        assert_eq!(
            resolve_active_player_name(None),
            Err(AdapterError::new("playerHandleForName requires a player name")),
            "a null Java string must fail before a roster lookup",
        );
        assert_eq!(
            resolve_active_player_name(Some("Bob")),
            Err(AdapterError::new(
                "playerHandleForName: no active player named \"Bob\"",
            )),
            "an unknown name must not mint a handle",
        );
        assert_eq!(
            resolve_active_player_uuid("05050505-0505-0505-0505-05050505050"),
            Err(AdapterError::new(
                "playerHandleForUuid: invalid UUID \"05050505-0505-0505-0505-05050505050\" (expected 36-character form)",
            )),
            "truncated UUIDs must fail before map lookup",
        );
        assert_eq!(active_player_count(), Ok(1));
        assert_eq!(active_player_handle_at(0), Ok(first));
        assert_eq!(
            active_player_handle_at(-1),
            Err(AdapterError::new(
                "activePlayerHandleAt requires a non-negative index",
            )),
            "a malformed Java index must fail before selecting a roster entry",
        );
        assert_eq!(
            resolve_resident_player_handle_is_active(first.to_bits()),
            Ok(true),
            "an active lifecycle handle resolves through the worker snapshot",
        );
        assert_eq!(resolve_resident_player_handle_name(first.to_bits()), Ok("Alice".to_owned()));
        assert_eq!(
            resolve_resident_player_handle_uuid(first.to_bits()),
            Ok("05050505-0505-0505-0505-050505050505".to_owned()),
        );
        assert_eq!(release_active_player_handle(&identity, &player), Some(first));
        assert_eq!(active_player_handle_for(&player), None);
        assert_eq!(active_player_count(), Ok(0));
        assert_eq!(
            active_player_handle_at(0),
            Err(AdapterError::new(
                "activePlayerHandleAt: index 0 is outside active player count 0",
            )),
            "a departed player must not remain enumerable after its handle is released",
        );
        assert_eq!(
            resolve_resident_player_handle_is_active(first.to_bits()),
            Err(AdapterError::new(
                "playerHandleIsActive: the referenced object no longer exists",
            )),
            "generation validation must reject the old bits before map lookup",
        );
        assert_eq!(
            resolve_resident_player_handle_name(first.to_bits()),
            Err(AdapterError::new(
                "playerHandleName: the referenced object no longer exists",
            )),
        );
        assert_eq!(
            resolve_active_player_uuid("05050505-0505-0505-0505-050505050505"),
            Err(AdapterError::new(
                "playerHandleForUuid: no active player with UUID 05050505-0505-0505-0505-050505050505",
            )),
            "disconnect removes the UUID reverse mapping rather than leaving a stale handle",
        );
        assert_eq!(
            resolve_active_player_name(Some("Alice")),
            Err(AdapterError::new(
                "playerHandleForName: no active player named \"Alice\"",
            )),
            "disconnect removes the name reverse mapping before a stale slot can be reused",
        );
        assert_eq!(
            resolve_active_player_name_ignoring_case(Some("alice")),
            Err(AdapterError::new(
                "playerHandleForNameIgnoringCase: no active player named \"alice\"",
            )),
            "disconnect removes the case-insensitive reverse mapping before slot reuse",
        );
        assert_eq!(
            resolve_active_player_profile(
                Some("Alice"),
                Some("05050505-0505-0505-0505-050505050505"),
            ),
            Err(AdapterError::new(
                "playerHandleForProfile: no active player named \"Alice\" with UUID 05050505-0505-0505-0505-050505050505",
            )),
            "disconnect removes the profile reverse mapping before a stale slot can be reused",
        );
        let replacement = active_player_handle(&identity, &player).expect("reusable slot");
        assert_ne!(replacement, first);
        assert_eq!(
            resolve_active_player_name(Some("Alice")),
            Ok(replacement),
            "a later roster observation resolves its new generation, never the departed handle",
        );
        assert_eq!(active_player_count(), Ok(1));
        assert_eq!(active_player_handle_at(0), Ok(replacement));
        assert_eq!(release_active_player_handle(&identity, &player), Some(replacement));
        assert_eq!(active_player_count(), Ok(0));
        RESIDENT_OBJECT_HANDLES.with(|slot| *slot.borrow_mut() = None);
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = None);
    }

    #[test]
    fn active_player_handle_enumeration_is_sorted_and_bounded() {
        let identity = lifecycle_identity("adapter", "adapter", "fixture.Adapter");
        let first_player = PlayerIdentity::new([2; 16], "Zulu");
        let second_player = PlayerIdentity::new([1; 16], "Beta");
        let third_player = PlayerIdentity::new([1; 16], "Alpha");
        RESIDENT_OBJECT_HANDLES.with(|slot| {
            *slot.borrow_mut() = Some(ObjectRegistry::with_capacity(3));
        });
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = Some(HashMap::new()));

        // Deliberately insert in the reverse of the contract order. The UUID
        // tie also proves that display name is the deterministic secondary key
        // instead of a hash-map winner.
        let first = active_player_handle(&identity, &first_player).expect("first profile handle");
        let second = active_player_handle(&identity, &second_player).expect("second profile handle");
        let third = active_player_handle(&identity, &third_player).expect("third profile handle");
        assert_eq!(active_player_count(), Ok(3));
        assert_eq!(
            resolve_active_player_uuid("01010101-0101-0101-0101-010101010101"),
            Err(AdapterError::new(
                "playerHandleForUuid: multiple active players with UUID 01010101-0101-0101-0101-010101010101",
            )),
            "duplicate UUIDs remain ambiguous to the unqualified resolver",
        );
        assert_eq!(active_player_handle_at(0), Ok(third));
        assert_eq!(active_player_handle_at(1), Ok(second));
        assert_eq!(active_player_handle_at(2), Ok(first));
        assert_eq!(
            active_player_handle_at(3),
            Err(AdapterError::new(
                "activePlayerHandleAt: index 3 is outside active player count 3",
            )),
            "an out-of-range Java index must not fabricate a handle",
        );
        assert!(release_active_player_handle(&identity, &third_player).is_some());
        assert_eq!(
            resolve_resident_player_handle_name(third.to_bits()),
            Err(AdapterError::new(
                "playerHandleName: the referenced object no longer exists",
            )),
            "releasing an indexed profile must stale its old generation",
        );
        assert_eq!(active_player_handle_at(0), Ok(second));
        assert!(release_active_player_handle(&identity, &second_player).is_some());
        assert!(release_active_player_handle(&identity, &first_player).is_some());
        RESIDENT_OBJECT_HANDLES.with(|slot| *slot.borrow_mut() = None);
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = None);
    }

    #[test]
    fn player_uuid_reverse_resolver_rejects_duplicate_active_profiles() {
        let identity = lifecycle_identity("adapter", "adapter", "fixture.Adapter");
        let first_player = PlayerIdentity::new([7; 16], "Alice");
        let second_player = PlayerIdentity::new([7; 16], "AliceRenamed");
        RESIDENT_OBJECT_HANDLES.with(|slot| {
            *slot.borrow_mut() = Some(ObjectRegistry::with_capacity(2));
        });
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = Some(HashMap::new()));
        active_player_handle(&identity, &first_player).expect("first profile handle");
        active_player_handle(&identity, &second_player).expect("second profile handle");
        assert_eq!(
            resolve_active_player_uuid("07070707-0707-0707-0707-070707070707"),
            Err(AdapterError::new(
                "playerHandleForUuid: multiple active players with UUID 07070707-0707-0707-0707-070707070707",
            )),
            "a duplicate UUID must fail rather than depend on map iteration order",
        );
        assert!(release_active_player_handle(&identity, &first_player).is_some());
        assert!(release_active_player_handle(&identity, &second_player).is_some());
        RESIDENT_OBJECT_HANDLES.with(|slot| *slot.borrow_mut() = None);
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = None);
    }

    #[test]
    fn player_name_reverse_resolver_rejects_duplicate_active_display_names() {
        let identity = lifecycle_identity("adapter", "adapter", "fixture.Adapter");
        let first_player = PlayerIdentity::new([8; 16], "Alice");
        let second_player = PlayerIdentity::new([9; 16], "Alice");
        RESIDENT_OBJECT_HANDLES.with(|slot| {
            *slot.borrow_mut() = Some(ObjectRegistry::with_capacity(2));
        });
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = Some(HashMap::new()));
        active_player_handle(&identity, &first_player).expect("first profile handle");
        active_player_handle(&identity, &second_player).expect("second profile handle");
        assert_eq!(
            resolve_active_player_name(Some("Alice")),
            Err(AdapterError::new(
                "playerHandleForName: multiple active players named \"Alice\"",
            )),
            "a duplicate display name must fail rather than depend on map iteration order",
        );
        assert!(release_active_player_handle(&identity, &first_player).is_some());
        assert!(release_active_player_handle(&identity, &second_player).is_some());
        RESIDENT_OBJECT_HANDLES.with(|slot| *slot.borrow_mut() = None);
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = None);
    }

    #[test]
    fn player_profile_reverse_resolver_disambiguates_the_copied_roster() {
        let identity = lifecycle_identity("adapter", "adapter", "fixture.Adapter");
        let first_player = PlayerIdentity::new([9; 16], "Alice");
        let second_player = PlayerIdentity::new([10; 16], "Alice");
        let renamed_player = PlayerIdentity::new([9; 16], "AliceRenamed");
        RESIDENT_OBJECT_HANDLES.with(|slot| {
            *slot.borrow_mut() = Some(ObjectRegistry::with_capacity(3));
        });
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = Some(HashMap::new()));
        let first = active_player_handle(&identity, &first_player).expect("first profile handle");
        let second = active_player_handle(&identity, &second_player).expect("second profile handle");
        let renamed = active_player_handle(&identity, &renamed_player).expect("renamed profile handle");
        assert_eq!(
            resolve_active_player_name(Some("Alice")),
            Err(AdapterError::new(
                "playerHandleForName: multiple active players named \"Alice\"",
            )),
            "the unqualified name control must prove the roster is ambiguous",
        );
        assert_eq!(
            resolve_active_player_uuid("09090909-0909-0909-0909-090909090909"),
            Err(AdapterError::new(
                "playerHandleForUuid: multiple active players with UUID 09090909-0909-0909-0909-090909090909",
            )),
            "the unqualified UUID control must prove the roster is ambiguous",
        );
        assert_eq!(
            resolve_active_player_profile(
                Some("Alice"),
                Some("09090909-0909-0909-0909-090909090909"),
            ),
            Ok(first),
        );
        assert_eq!(
            resolve_active_player_profile(
                Some("Alice"),
                Some("0a0a0a0a-0a0a-0a0a-0a0a-0a0a0a0a0a0a"),
            ),
            Ok(second),
        );
        assert_eq!(
            resolve_active_player_profile(
                Some("AliceRenamed"),
                Some("09090909-0909-0909-0909-090909090909"),
            ),
            Ok(renamed),
            "the full copied profile, unlike either field alone, has one worker-owned handle",
        );
        assert!(release_active_player_handle(&identity, &first_player).is_some());
        assert!(release_active_player_handle(&identity, &second_player).is_some());
        assert!(release_active_player_handle(&identity, &renamed_player).is_some());
        RESIDENT_OBJECT_HANDLES.with(|slot| *slot.borrow_mut() = None);
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = None);
    }

    #[test]
    fn case_insensitive_player_name_resolver_rejects_case_collisions() {
        let identity = lifecycle_identity("adapter", "adapter", "fixture.Adapter");
        let first_player = PlayerIdentity::new([10; 16], "Alice");
        let second_player = PlayerIdentity::new([11; 16], "aLiCe");
        RESIDENT_OBJECT_HANDLES.with(|slot| {
            *slot.borrow_mut() = Some(ObjectRegistry::with_capacity(2));
        });
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = Some(HashMap::new()));
        active_player_handle(&identity, &first_player).expect("first profile handle");
        active_player_handle(&identity, &second_player).expect("second profile handle");
        assert_eq!(
            resolve_active_player_name_ignoring_case(Some("ALICE")),
            Err(AdapterError::new(
                "playerHandleForNameIgnoringCase: multiple active players named \"ALICE\"",
            )),
            "case variants must not select a hash-map iteration winner",
        );
        assert!(release_active_player_handle(&identity, &first_player).is_some());
        assert!(release_active_player_handle(&identity, &second_player).is_some());
        RESIDENT_OBJECT_HANDLES.with(|slot| *slot.borrow_mut() = None);
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = None);
    }

    #[test]
    fn player_name_prefix_resolver_is_bounded_unambiguous_and_generation_checked() {
        let identity = lifecycle_identity("adapter", "adapter", "fixture.Adapter");
        let alice = PlayerIdentity::new([12; 16], "Alice");
        let alina = PlayerIdentity::new([13; 16], "Alina");
        let bob = PlayerIdentity::new([14; 16], "Bob");
        RESIDENT_OBJECT_HANDLES.with(|slot| {
            *slot.borrow_mut() = Some(ObjectRegistry::with_capacity(3));
        });
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = Some(HashMap::new()));
        let alice_handle = active_player_handle(&identity, &alice).expect("Alice handle");
        active_player_handle(&identity, &alina).expect("Alina handle");
        let bob_handle = active_player_handle(&identity, &bob).expect("Bob handle");

        assert_eq!(
            resolve_active_player_name_prefix(None),
            Err(AdapterError::new(
                "playerHandleForNamePrefix requires a player name prefix",
            )),
            "a null Java string must fail before roster lookup",
        );
        assert_eq!(
            resolve_active_player_name_prefix(Some("")),
            Err(AdapterError::new(
                "playerHandleForNamePrefix requires a non-empty player name prefix",
            )),
            "an empty prefix must not turn the whole roster into an implicit query",
        );
        assert_eq!(
            resolve_active_player_name_prefix(Some("Cara")),
            Err(AdapterError::new(
                "playerHandleForNamePrefix: no active player whose name starts with \"Cara\"",
            )),
            "an unknown prefix must not mint a handle",
        );
        assert_eq!(
            resolve_active_player_name_prefix(Some("Ali")),
            Err(AdapterError::new(
                "playerHandleForNamePrefix: multiple active players whose names start with \"Ali\"",
            )),
            "a shared prefix must not select a hash-map iteration winner",
        );
        assert_eq!(
            resolve_active_player_name_prefix(Some("Bo")),
            Ok(bob_handle),
            "the unique copied prefix resolves the existing worker handle",
        );

        assert_eq!(release_active_player_handle(&identity, &bob), Some(bob_handle));
        assert_eq!(
            resolve_active_player_name_prefix(Some("Bo")),
            Err(AdapterError::new(
                "playerHandleForNamePrefix: no active player whose name starts with \"Bo\"",
            )),
            "a disconnected profile must leave no stale prefix mapping",
        );
        let replacement = active_player_handle(&identity, &bob).expect("replacement handle");
        assert_ne!(replacement, bob_handle, "slot reuse must advance generation");
        assert_eq!(resolve_active_player_name_prefix(Some("Bo")), Ok(replacement));
        assert_eq!(
            resolve_resident_player_handle_name(bob_handle.to_bits()),
            Err(AdapterError::new(
                "playerHandleName: the referenced object no longer exists",
            )),
            "the old prefix result must fail generation validation after slot reuse",
        );

        assert!(release_active_player_handle(&identity, &alice).is_some());
        assert!(release_active_player_handle(&identity, &alina).is_some());
        assert!(release_active_player_handle(&identity, &bob).is_some());
        assert_ne!(alice_handle, replacement);
        RESIDENT_OBJECT_HANDLES.with(|slot| *slot.borrow_mut() = None);
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = None);
    }

    #[test]
    fn deadline_rejects_a_late_completion_and_remains_terminal() {
        let mut host = AdapterHost::spawn(Duration::from_secs(2), |commands, events, _port, _, _| {
            events.send(Ok(AdapterEvent::Ready)).unwrap();
            let _ = commands.recv();
        }).unwrap();
        // An already-delivered event must not rescue an operation whose host
        // deadline elapsed. Backdate the clock state instead of sleeping.
        let limit = Instant::now() + Duration::from_secs(2);
        while host.events.try_recv().is_err() {
            assert!(Instant::now() < limit);
            std::thread::yield_now();
        }
        let (sender, receiver) = sync_channel(1);
        sender.send(Ok(AdapterEvent::Ready)).unwrap();
        host.events = receiver;
        host.state = State::Loading(Instant::now() - Duration::from_secs(3));
        let error = host.poll().expect_err("late readiness must time out");
        assert!(error.to_string().contains("exceeded"));
        assert_eq!(host.poll(), Err(error));
        assert!(host.dispatch_tick(1).is_err());
        assert_eq!(host.service_pending(1, |_| panic!("terminal host must not service")), 0);
    }

    #[test]
    fn worker_errors_preserve_the_named_failure() {
        let mut host = AdapterHost::spawn(Duration::from_secs(2), |commands, events, _port, _, _| {
            events.send(Err(AdapterError::new("example.Adapter.onTick: missing member"))).unwrap();
            let _ = commands.recv();
        }).unwrap();
        let limit = Instant::now() + Duration::from_secs(2);
        loop {
            match host.poll() {
                Err(error) => {
                    assert_eq!(error.to_string(), "example.Adapter.onTick: missing member");
                    break;
                }
                Ok(_) => assert!(Instant::now() < limit),
            }
            std::thread::yield_now();
        }
    }

    #[test]
    fn server_tick_reads_run_on_the_host_and_preserve_zero() {
        let host_thread = std::thread::current().id();
        let mut host = AdapterHost::spawn(
            Duration::from_secs(2),
            move |_commands, events, _port, _write_port, tick_port| {
                assert_ne!(std::thread::current().id(), host_thread);
                events.send(Ok(AdapterEvent::Ready)).unwrap();
                assert_eq!(tick_port.request(()).unwrap(), Ok(0));
            },
        )
        .unwrap();
        assert_eq!(await_event(&mut host), AdapterEvent::Ready);
        let limit = Instant::now() + Duration::from_secs(2);
        while host.service_pending_server_tick(1, || Ok(0)) == 0 {
            assert!(Instant::now() < limit, "server tick query did not reach the host");
            std::thread::yield_now();
        }
    }

    #[test]
    fn block_writes_run_on_the_host_and_keep_the_full_request() {
        let host_thread = std::thread::current().id();
        let mut host = AdapterHost::spawn(
            Duration::from_secs(2),
            move |_commands, events, _port, write_port, _tick_port| {
                assert_ne!(std::thread::current().id(), host_thread);
                events.send(Ok(AdapterEvent::Ready)).unwrap();
                assert_eq!(
                    write_port.request(BlockStateWrite {
                        x: -17,
                        y: 64,
                        z: 33,
                        state_id: 1234,
                    }).unwrap(),
                    Ok(())
                );
            },
        ).unwrap();
        assert_eq!(await_event(&mut host), AdapterEvent::Ready);
        let limit = Instant::now() + Duration::from_secs(2);
        while host.service_pending_block_writes(1, |write| {
            assert_eq!(std::thread::current().id(), host_thread);
            assert_eq!(
                write,
                BlockStateWrite { x: -17, y: 64, z: 33, state_id: 1234 }
            );
            Ok(())
        }) == 0 {
            assert!(Instant::now() < limit, "block write did not reach the host");
            std::thread::yield_now();
        }
    }

    #[test]
    fn resident_block_change_subscriptions_activate_and_cleanup_in_stable_order() {
        let alpha = lifecycle_identity("alpha", "one", "alpha.Main");
        let beta = lifecycle_identity("beta", "one", "beta.Main");
        let mut subscriptions = ResidentBlockChangeSubscriptions::<u8>::default();
        assert_eq!(subscriptions.register(alpha.clone(), 10).unwrap(), 0);
        assert_eq!(subscriptions.register(beta.clone(), 20).unwrap(), 1);
        assert_eq!(subscriptions.register(alpha.clone(), 30).unwrap(), 2);
        assert_eq!(subscriptions.active_entries().count(), 0);

        assert_eq!(subscriptions.activate(&alpha), 2);
        let active: Vec<_> = subscriptions
            .active_entries()
            .map(|(number, identity, listener)| (number, identity.name.as_str(), *listener))
            .collect();
        assert_eq!(active, vec![(0, "alpha", 10), (2, "alpha", 30)]);

        assert_eq!(subscriptions.clear(&alpha), 2);
        assert_eq!(subscriptions.register(beta.clone(), 40).unwrap(), 3);
        assert_eq!(subscriptions.activate(&beta), 2);
        let active: Vec<_> = subscriptions
            .active_entries()
            .map(|(number, identity, listener)| (number, identity.name.as_str(), *listener))
            .collect();
        assert_eq!(active, vec![(1, "beta", 20), (3, "beta", 40)]);
    }

    #[test]
    fn resident_block_change_listener_failures_do_not_stop_later_listeners() {
        let mut calls = Vec::new();
        let failures = dispatch_isolated_listeners(
            vec![
                (0, "alpha".to_owned(), 10_u8),
                (1, "bravo".to_owned(), 20_u8),
                (2, "charlie".to_owned(), 30_u8),
            ],
            |listener| {
                calls.push(*listener);
                if *listener == 20 {
                    Err("listener threw".to_owned())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(calls, vec![10, 20, 30]);
        assert_eq!(
            failures,
            vec![ResidentBlockChangeListenerFailure {
                registration: 1,
                plugin_name: "bravo".to_owned(),
                detail: "listener threw".to_owned(),
            }],
        );
    }

    #[test]
    fn resident_block_change_subscriptions_have_a_worker_bound() {
        let identity = lifecycle_identity("bounded", "one", "bounded.Main");
        let mut subscriptions = ResidentBlockChangeSubscriptions::<()>::default();
        for _ in 0..MAX_RESIDENT_BLOCK_CHANGE_SUBSCRIPTIONS {
            subscriptions
                .register(identity.clone(), ())
                .expect("entries below the worker bound are accepted");
        }
        let error = subscriptions
            .register(identity, ())
            .expect_err("the worker bound must reject another listener");
        assert!(error.to_string().contains("subscription limit 64 exceeded"));
    }

    #[test]
    fn callback_block_handles_are_opaque_and_cleanup_makes_old_bits_stale() {
        let identity = lifecycle_identity("alpha", "one", "alpha.Main");
        let other_identity = lifecycle_identity("bravo", "one", "bravo.Main");
        let change = BlockStateWrite {
            x: 11,
            y: 1,
            z: 4,
            state_id: 1234,
        };
        RESIDENT_OBJECT_HANDLES.with(|slot| {
            *slot.borrow_mut() = Some(ObjectRegistry::with_capacity(2));
        });
        CURRENT_RESIDENT_BLOCK_HANDLE.with(|slot| *slot.borrow_mut() = None);
        CURRENT_RESIDENT_PLAYER_HANDLE.with(|slot| *slot.borrow_mut() = None);

        let first = resident_block_handle(&identity, change).expect("first block handle");
        let other = resident_block_handle(
            &other_identity,
            BlockStateWrite { x: 12, ..change },
        )
        .expect("other owner's block handle");
        assert_eq!(first.kind(), ObjectKind::Block);
        assert_eq!(
            RESIDENT_OBJECT_HANDLES.with(|slot| {
                slot.borrow()
                    .as_ref()
                    .expect("registry")
                    .resolve(first, ObjectKind::Block)
                    .and_then(|object| match object {
                        ResidentObject::Block { position, .. } => Ok(*position),
                        ResidentObject::Player { .. } => Err(ResolveError::KindMismatch {
                            expected: ObjectKind::Block,
                            actual: ObjectKind::Player,
                        }),
                    })
            }),
            Ok((11, 1, 4)),
        );
        assert_eq!(
            current_resident_block_handle(),
            Err(AdapterError::new(
                "currentBlockHandle requires an active resident block-change callback",
            )),
        );
        {
            let _guard = ResidentBlockHandleGuard::enter(Some(first));
            assert_eq!(current_resident_block_handle().expect("callback handle"), first);
        }
        assert_eq!(
            current_resident_block_handle(),
            Err(AdapterError::new(
                "currentBlockHandle requires an active resident block-change callback",
            )),
        );
        assert_eq!(
            resolve_resident_block_handle(first.to_bits(), "blockHandlePosition")
                .expect("live handle position"),
            (11, 1, 4),
        );
        assert_eq!(
            resident_block_handle_coordinate(first.to_bits(), 0, "blockHandleX"),
            Ok(11),
        );
        assert_eq!(
            resident_block_handle_coordinate(first.to_bits(), 1, "blockHandleY"),
            Ok(1),
        );
        assert_eq!(
            resident_block_handle_coordinate(first.to_bits(), 2, "blockHandleZ"),
            Ok(4),
            "coordinates are copied from the worker registry without a world request",
        );

        assert_eq!(release_resident_handles(&identity), 1);
        assert_eq!(
            resolve_resident_block_handle(first.to_bits(), "blockHandlePosition"),
            Err(AdapterError::new(
                "blockHandlePosition: the referenced object no longer exists",
            )),
        );
        assert_eq!(
            resident_block_handle_coordinate(first.to_bits(), 0, "blockHandleX"),
            Err(AdapterError::new(
                "blockHandleX: the referenced object no longer exists",
            )),
            "released bits must not resolve through a recycled block slot",
        );
        assert_eq!(
            RESIDENT_OBJECT_HANDLES.with(|slot| {
                slot.borrow()
                    .as_ref()
                    .expect("registry")
                    .resolve(first, ObjectKind::Block)
                    .map(|_| ())
            }),
            Err(ResolveError::Stale),
        );
        assert_eq!(
            RESIDENT_OBJECT_HANDLES.with(|slot| {
                slot.borrow()
                    .as_ref()
                    .expect("registry")
                    .resolve(other, ObjectKind::Block)
                    .and_then(|object| match object {
                        ResidentObject::Block { position, .. } => Ok(*position),
                        ResidentObject::Player { .. } => Err(ResolveError::KindMismatch {
                            expected: ObjectKind::Block,
                            actual: ObjectKind::Player,
                        }),
                    })
            }),
            Ok((12, 1, 4)),
            "disabling one entry must not invalidate another entry's handle",
        );
        let replacement = resident_block_handle(&identity, change).expect("replacement handle");
        assert_ne!(replacement, first);
        RESIDENT_OBJECT_HANDLES.with(|slot| *slot.borrow_mut() = None);
        CURRENT_RESIDENT_BLOCK_HANDLE.with(|slot| *slot.borrow_mut() = None);
        CURRENT_RESIDENT_PLAYER_HANDLE.with(|slot| *slot.borrow_mut() = None);
    }

    #[test]
    fn block_handle_state_read_uses_the_bounded_port_after_generation_check() {
        let identity = lifecycle_identity("alpha", "one", "alpha.Main");
        let change = BlockStateWrite {
            x: 11,
            y: 1,
            z: 4,
            state_id: 1234,
        };
        let (port, servicer) = channel(Duration::from_secs(1));
        let (result_sender, result_receiver) = sync_channel(1);
        let worker = std::thread::spawn(move || {
            RESIDENT_OBJECT_HANDLES.with(|slot| {
                *slot.borrow_mut() = Some(ObjectRegistry::with_capacity(1));
            });
            CALLBACK_PORT.with(|slot| *slot.borrow_mut() = Some(port));
            let handle = resident_block_handle(&identity, change).expect("block handle");
            let result = resident_block_handle_state_id(handle.to_bits());
            assert_eq!(release_resident_handles(&identity), 1);
            let stale = resident_block_handle_state_id(handle.to_bits());
            CALLBACK_PORT.with(|slot| *slot.borrow_mut() = None);
            RESIDENT_OBJECT_HANDLES.with(|slot| *slot.borrow_mut() = None);
            result_sender.send((result, stale)).expect("state results");
        });
        let limit = Instant::now() + Duration::from_secs(1);
        while servicer.service_all_pending(1, |query| {
            assert_eq!(query, BlockStateQuery { x: 11, y: 1, z: 4 });
            Ok(422)
        }) == 0 {
            assert!(Instant::now() < limit, "worker did not request a block state");
            std::thread::yield_now();
        }
        let (result, stale) = result_receiver.recv().expect("state results");
        assert_eq!(result, Ok(422));
        assert_eq!(
            stale,
            Err(AdapterError::new(
                "blockHandleStateId: the referenced object no longer exists",
            )),
            "a stale handle must fail before a host request is made",
        );
        worker.join().expect("worker joins");
    }

    #[test]
    fn callback_player_handles_are_typed_generation_checked_and_bounded() {
        let identity = lifecycle_identity("alpha", "one", "alpha.Main");
        let other_identity = lifecycle_identity("bravo", "one", "bravo.Main");
        let player = PlayerIdentity::new([7; 16], "Alice");
        let other_player = PlayerIdentity::new([8; 16], "Bob");
        RESIDENT_OBJECT_HANDLES.with(|slot| {
            *slot.borrow_mut() = Some(ObjectRegistry::with_capacity(2));
        });
        CURRENT_RESIDENT_PLAYER_HANDLE.with(|slot| *slot.borrow_mut() = None);
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = Some(HashMap::new()));

        let first = resident_player_handle(&identity, &player).expect("first player handle");
        assert_eq!(first.kind(), ObjectKind::Player);
        assert_eq!(player.uuid(), [7; 16]);
        assert_eq!(player.name(), "Alice");
        assert_eq!(resolve_resident_player_handle_name(first.to_bits()), Ok("Alice".to_owned()));
        assert_eq!(
            resolve_resident_player_handle_uuid(first.to_bits()),
            Ok("07070707-0707-0707-0707-070707070707".to_owned()),
            "the UUID is fixed-size copied profile data, not a Java server object",
        );
        assert_eq!(
            resolve_resident_player_handle_is_active(first.to_bits()),
            Ok(false),
            "a callback-only handle does not imply a reconciled active lifecycle entry",
        );
        assert_eq!(
            RESIDENT_OBJECT_HANDLES.with(|slot| {
                slot.borrow()
                    .as_ref()
                    .expect("registry")
                    .resolve(first, ObjectKind::Block)
                    .map(|_| ())
            }),
            Err(ResolveError::KindMismatch {
                expected: ObjectKind::Block,
                actual: ObjectKind::Player,
            }),
            "a player handle must not resolve through the block kind",
        );
        assert_eq!(
            current_resident_player_handle(),
            Err(AdapterError::new(
                "currentPlayerHandle requires an active resident block-change callback with a player",
            )),
        );
        {
            let _guard = ResidentPlayerHandleGuard::enter(Some(first));
            assert_eq!(current_resident_player_handle().expect("callback player"), first);
        }
        assert_eq!(
            current_resident_player_handle(),
            Err(AdapterError::new(
                "currentPlayerHandle requires an active resident block-change callback with a player",
            )),
        );

        assert_eq!(
            resident_player_handle(&other_identity, &other_player)
                .expect("second player fits")
                .kind(),
            ObjectKind::Player,
        );
        let full = resident_player_handle(&identity, &other_player)
            .expect_err("the bounded shared registry must reject a third live object");
        assert!(full.to_string().contains("capacity 2 exceeded"), "{full}");

        assert_eq!(release_resident_handles(&identity), 1);
        assert_eq!(
            resolve_resident_player_handle_name(first.to_bits()),
            Err(AdapterError::new(
                "playerHandleName: the referenced object no longer exists",
            )),
        );
        assert_eq!(
            resolve_resident_player_handle_uuid(first.to_bits()),
            Err(AdapterError::new(
                "playerHandleUuid: the referenced object no longer exists",
            )),
            "a stale handle cannot resolve a later player's profile",
        );
        let replacement = resident_player_handle(&identity, &player).expect("slot is reusable");
        assert_ne!(replacement, first, "release must advance the player generation");
        assert_eq!(
            RESIDENT_OBJECT_HANDLES.with(|slot| {
                slot.borrow_mut()
                    .as_mut()
                    .expect("registry")
                    .clear()
            }),
            2,
            "worker cleanup must release the replacement and the other owner's handle",
        );
        assert_eq!(
            RESIDENT_OBJECT_HANDLES.with(|slot| {
                slot.borrow()
                    .as_ref()
                    .expect("registry")
                    .resolve(replacement, ObjectKind::Player)
                    .map(|_| ())
            }),
            Err(ResolveError::Stale),
            "worker cleanup must invalidate every outstanding player handle",
        );
        RESIDENT_OBJECT_HANDLES.with(|slot| *slot.borrow_mut() = None);
        CURRENT_RESIDENT_PLAYER_HANDLE.with(|slot| *slot.borrow_mut() = None);
        ACTIVE_PLAYER_HANDLES.with(|slot| *slot.borrow_mut() = None);
    }
}
