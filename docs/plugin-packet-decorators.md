# Packet decorators: the version-locked ProtocolLib-class escape hatch

## What it is

A wrapper struct implementing `ServerProtocol` (server) or `VersionAdapter` (client) around a
concrete protocol family's own type, forwarding most calls unchanged and intercepting the ones a
plugin author cares about. This is the one route in this codebase to ProtocolLib-class packet
access — see, drop, rewrite, or append traffic in either direction — at the cost of depending on a
concrete version crate directly instead of the version-free shared crates every other plugin
surface targets. `crates/versions/26.2/tests/server/server_protocol_decorator_escape_hatch.rs` and
`crates/versions/26.2/tests/singleplayer_lan/client_adapter_decorator_escape_hatch.rs` are executable
proof, one test per verb per direction, each with a control showing the undecorated protocol's own
behaviour first.

## How it works

### Why a plain wrapper struct works at all

Both `ServerProtocol` (`lodestone_server::protocol`) and `VersionAdapter`
(`lodestone_model::adapter`) are object-safe traits with no generic methods and no `Self` in
argument position, so both support the ordinary decorator shape: a struct holding the wrapped value
plus whatever hook state a test or plugin needs, implementing the trait itself, calling through to
the wrapped value's own methods and modifying the request or the result around that call.
`ServerProtocol` even ships this pattern in its own crate — `impl<P: ServerProtocol + ?Sized>
ServerProtocol for Box<P>` exists so a boxed protocol can be handed to `IntegratedServer::open_*`,
and its own doc comment names the hazard every hand-written decorator inherits (next section).

### The forwarding hazard: most methods have a default, and the default is not "do nothing safely"

Both traits declare only a handful of methods with no default body — seven for `ServerProtocol`
(`decode`, `login_success`, `begin_configuration`, `begin_play`, `begin_chunk_batch`,
`encode_chunk`, `end_chunk_batch`) and seven for `VersionAdapter` (`protocol_version`,
`minecraft_versions`, `supports`, `begin_login`, `handle_packet`, `encode_action`,
`build_encryption_response`). Those seven are the only methods a decorator is *forced* to write, so
they are the only ones the compiler protects. Every other method — `encode_system_chat`,
`welcome_message`, `encode_teleport`, `entity_dimensions`, `block_hardness`, and dozens more — has a
default body, almost always `ServerDirective::None` or an empty `Vec`/`None`. **A decorator that
does not explicitly forward one of these methods silently answers with the trait's own default
instead of the wrapped protocol's real behaviour.** A decorator that forwards only the required
seven will join a client (chunks arrive, `begin_play` runs), but every optional packet family —
keep-alives, boss bars, attribute updates, the welcome message — simply stops firing, with no error
anywhere. The fix is mechanical and has no shortcut: forward every method the decorator does not
intend to hook, one line each, the same way `impl ServerProtocol for Box<P>` does for the whole
trait.

### The verbs, and which direction supports which

Both traits have one direction whose method returns a **batch** and one whose method returns **at
most one value**:

| trait | inbound method | outbound method |
|---|---|---|
| `ServerProtocol` | `decode(..) -> ServerBound` (one value) | `welcome_message(..) -> Vec<ServerDirective>` and siblings (a batch); `encode_system_chat(..) -> ServerDirective` (one value) |
| `VersionAdapter` | `handle_packet(..) -> Result<Vec<Directive>, _>` (a batch) | `encode_action(..) -> Result<Option<(i32, Vec<u8>)>, _>` (at most one) |

**Drop and rewrite work identically in both directions**, because both only need to change or
discard a single value already in hand: a decorator's `decode` can turn a decoded `ServerBound::Chat`
into `ServerBound::Ignored` (drop) or rewrite its `message` field (rewrite) before returning it; its
`encode_action` can return `Ok(None)` (drop) or delegate to the wrapped adapter with a rewritten
`ClientAction` (rewrite).

**Append only works where the method returns a batch.** A decorator's `welcome_message` can call the
wrapped protocol's own `welcome_message()`, then push one more `ServerDirective::Send` built from
that same protocol's own `encode_system_chat` — a packet the wrapped protocol never sent on its own.
The same shape works on `VersionAdapter::handle_packet`, pushing an extra `Directive::Emit(..)` the
real server never sent. Neither `ServerProtocol::decode` nor `VersionAdapter::encode_action` can be
made to do this: both return exactly one value per call, so there is no way to turn one inbound
packet into two inbound actions, or queue a second outbound packet from a single `encode_action`
call. This is a signature constraint, not a missing feature — matching the audit's phrasing, an
inbound decorator "can see an unknown packet's bytes but cannot inject a new kind of action," because
`ServerBound` and `ClientAction` are both closed enums a decorator's crate cannot add variants to,
and the method that produces them from one call site can only produce one.

### What "sees both directions" means concretely

A `ServerProtocol` decorator sees every inbound payload through `decode` and every outbound batch
through the `Vec<ServerDirective>`-returning methods and every single-packet method
(`encode_system_chat`, `encode_teleport`, …) — genuinely both directions of the connection, at the
one seam `IntegratedServer` calls through. A `VersionAdapter` decorator sees the mirror image on the
client: every inbound packet through `handle_packet`, every outbound action through `encode_action`.
Passed to `ClientBuilder::new` as a boxed trait object exactly like an undecorated adapter, a headless
bot built this way gets ProtocolLib-class visibility with no server-side change at all.

## How to change it, and the gotchas

- **Version-locked, and that is the real cost.** A decorator forwards `ServerProtocol`/
  `VersionAdapter` — traits every protocol family implements — but a hook written against one
  family's concrete behaviour does not transfer to another. `V26.2`'s `V770ServerProtocol` sends
  chat through `encode_system_chat` and a join-time welcome line through `welcome_message`; an older
  family may format chat differently, leave `welcome_message` at its empty default, or shape
  `ServerBound::Chat`'s fields differently (no `salt`/`signature` at all pre-1.19). A decorator
  written for one family compiles against any family (the trait is version-free) but its hooks may
  silently match nothing, or match the wrong thing, against a different one — there is no compiler
  check for "this hook still does something meaningful here." Never assume a decorator ports by
  recompiling against a different version crate; re-verify its assumptions against that family's own
  `server_protocol.rs`/`adapter.rs` first.
- **Forward every method you are not hooking**, using `impl<P: ServerProtocol + ?Sized>
  ServerProtocol for Box<P>` (in `lodestone_server::protocol`) as the exhaustive template for the
  server side. Test only the required seven methods and the decorator will compile, join, and
  silently disable every optional packet family nobody wrote a test for.
- **Unsandboxed.** Nothing here validates a rewritten or appended packet's bytes — a decorator can
  build a `ServerDirective::Send` with a `packet_id`/`payload` that is not a valid encoding of
  anything, and it goes straight to the wire. The sanctioned, shared-crate plugin surface
  (`ActionVetoes`/`EgressFilters`, see [`packet-wiring.md`](./packet-wiring.md)) never hands a
  plugin raw bytes for exactly this reason; a decorator opts out of that safety net by construction.
- **`ServerBound` and `ClientAction` are both closed enums** from a decorator's crate — a decorator
  can drop, rewrite, or observe a value already constructed by the wrapped protocol/adapter, but it
  cannot manufacture a wire action of a kind the wrapped protocol never decodes in the first place.
- **The reentrancy rules elsewhere in this codebase do not relax here.** A decorator's methods run
  on the same connection task the wrapped protocol/adapter's would; nothing about the wrapper
  changes what thread or lock context they run under.

## Configuration

None of its own. A decorator is a plain Rust generic/trait-object wrapper — no manifest, no feature
flag, no environment variable. The only "opt-in" is the `Cargo.toml` edge onto a concrete version
crate, which is also what makes it version-locked.

## Dependencies

- Server side: `lodestone-server` (`ServerProtocol`, `ServerDirective`, `ServerBound`,
  `IntegratedServer`) plus a version crate directly (e.g. `lodestone-v26-2`, for
  `V770ServerProtocol`) for the concrete type to wrap.
- Client side: `lodestone-client` (`ClientBuilder`) and `lodestone-model` (`VersionAdapter`,
  `Directive`, `ClientAction`, `ClientEvent`) plus the same version crate for the concrete adapter
  constructor (`lodestone_v26_2::adapter()`/`V770Adapter`).
- Neither depends on `lodestone-registry`: a decorator names its concrete version crate directly,
  which is the version-locking cost, not a registry lookup.

## See also

- [`packet-wiring.md`](./packet-wiring.md) — the sanctioned, version-free, sandboxed-by-construction
  plugin surface (`ActionVetoes`/`EgressFilters`) this escape hatch sits outside of.
- [`plugin-api.md`](./plugin-api.md) — the client-side plugin surface as a whole, and where the
  packet-observation ceiling this document's escape hatch sits outside of is decided.
- [`plugin-capability-audit.md`](./plugin-capability-audit.md) — the capability-by-capability audit
  that first named this escape hatch and the tests here confirm.
