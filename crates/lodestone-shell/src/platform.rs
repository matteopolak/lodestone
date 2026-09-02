//! `crate::platform` — the shell's native/browser seam for the things a browser
//! does not have.
//!
//! The shell is one application compiled for two targets: a native `winit`/`wgpu`
//! window, and `wasm32-unknown-unknown` inside a browser tab (consumed by `web/`).
//! Almost all of it is portable already. This module holds the handful of
//! primitives that are *not*, so the rest of the crate names one symbol and stops
//! caring.
//!
//! # The clock
//!
//! [`Instant`] and [`epoch_duration`] used to be defined here, with the reasoning
//! now in `lodestone-time`'s crate docs — moved there so every crate in the
//! workspace has exactly one clock to depend on rather than a copy each. This
//! module just re-exports them, so the crate's 59 existing call sites
//! (`crate::platform::Instant`, `crate::platform::epoch_duration`) keep compiling
//! unchanged. Read `lodestone_time`'s crate-level docs for the "why `web_time`",
//! "why a dedicated crate" and "why `SystemTime::now()` is the sneaky one"
//! reasoning; nothing about it changed, only where it lives.
pub use lodestone_time::{Instant, epoch_duration};

/// Small-document persistence: `options.json` and its two siblings.
///
/// # What it is
///
/// The shell persists three small JSON documents — `Options` (44 live option rows /
/// 66 live cells, including eleven sound volumes, `fov`, both glint knobs and
/// `cloudStatus`), `HiddenPlayers` and `SelectedPacks` — each through the same
/// `load_from(&Path)` / `save_to(&Path)` shape. This module is the one place those
/// six methods touch storage.
///
/// # Why it is not left to degrade
///
/// `std::fs` does not crash in a browser, it returns `Err(Unsupported)` (measured).
/// For most of the shell that is honest absence and perfectly acceptable — "no
/// saves", "no resource packs". **For options it is not.** Every read would miss and
/// every write would fail, so a browser player would set four dozen working controls
/// and lose all of them on reload, with no error anywhere. Silent, total, and
/// indistinguishable from "the settings screen is broken". That is a defect, not a
/// degradation, which is why this seam exists and the `saves.rs` one does not.
///
/// # How it works
///
/// `localStorage` is the natural fit and the reason is structural rather than
/// convenient: it is **synchronous** and **string-keyed**, which is exactly the shape
/// `load_from`/`save_to` already have. IndexedDB would be the wrong choice — it is
/// async, so it could not sit behind these signatures without making every caller
/// async too. Capacity is ~5 MB per origin against three documents measured in
/// hundreds of bytes.
///
/// The key is the **full path string**, prefixed. That keeps the seam a pure
/// substitution: callers still pass the `Path` they always did, tests still pass
/// their own temp paths, and two documents with the same basename in different
/// directories still cannot collide. It is stable across reloads because
/// `lodestone_auth::paths::data_dir` derives the same default every time on wasm
/// (every `var_os` yields `None` there, so it takes the no-`HOME` branch).
///
/// # How to change it
///
/// Keep [`read_text`] returning `io::Result` rather than `Option`. "Absent" and
/// "storage refused" are different: `localStorage` genuinely throws in a Safari
/// private window and when a quota is exceeded, and a caller that collapses both to
/// `None` turns a *refusal* into a silent reset to defaults — the same defect in a
/// smaller box.
#[cfg(target_arch = "wasm32")]
pub mod store {
    use std::io::{Error, ErrorKind};
    use std::path::Path;

    /// Prefix on every key, so the shell's documents are identifiable in devtools
    /// and cannot collide with anything else on the origin.
    const KEY_PREFIX: &str = "lodestone:";

    fn key_for(path: &Path) -> String {
        format!("{KEY_PREFIX}{}", path.display())
    }

    fn storage() -> Result<web_sys::Storage, Error> {
        web_sys::window()
            .ok_or_else(|| Error::new(ErrorKind::Unsupported, "no window: not a browser page"))?
            .local_storage()
            .map_err(|e| {
                // Thrown rather than returned-null in a Safari private window and
                // where the origin is opaque.
                Error::other(format!("localStorage unavailable: {e:?}"))
            })?
            .ok_or_else(|| Error::new(ErrorKind::Unsupported, "localStorage is disabled"))
    }

    /// Read a document, or `Err(NotFound)` when it has never been written.
    ///
    /// # Errors
    /// `NotFound` when the key is absent; `Unsupported` when there is no
    /// `localStorage` at all; otherwise the storage error.
    pub fn read_text(path: &Path) -> Result<String, Error> {
        let key = key_for(path);
        storage()?
            .get_item(&key)
            .map_err(|e| Error::other(format!("localStorage read {key}: {e:?}")))?
            .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no stored {key}")))
    }

    /// Write a document, replacing any previous value.
    ///
    /// # Errors
    /// `Unsupported` with no `localStorage`; otherwise the storage error, which in
    /// practice is a quota failure and is worth surfacing rather than swallowing.
    pub fn write_text(path: &Path, text: &str) -> Result<(), Error> {
        let key = key_for(path);
        storage()?
            .set_item(&key, text)
            .map_err(|e| Error::other(format!("localStorage write {key}: {e:?}")))
    }
}

/// Small-document persistence. Native: plain `std::fs`, unchanged.
///
/// See the browser arm's docs for why this seam exists at all. On this target it is a
/// direct forward, so native behaviour — including the `create_dir_all` before a
/// write, which a browser has no analogue for and does not need — is exactly what it
/// was.
#[cfg(not(target_arch = "wasm32"))]
pub mod store {
    use std::io::Error;
    use std::path::Path;

    /// Read a document.
    ///
    /// # Errors
    /// The underlying I/O error, including `NotFound` when it does not exist.
    pub fn read_text(path: &Path) -> Result<String, Error> {
        std::fs::read_to_string(path)
    }

    /// Write a document, creating its parent directory first.
    ///
    /// # Errors
    /// The underlying I/O error if the directory cannot be created or the file
    /// cannot be written.
    pub fn write_text(path: &Path, text: &str) -> Result<(), Error> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, text)
    }
}

/// The browser's asset byte source.
///
/// # What it is
///
/// Native resolves assets by *path*: `resources.rs` walks for a pack root and
/// `std::fs::read`s `client.jar` and `generated/reports/blocks.json` out of it. A
/// browser has no filesystem — measured: `std::fs::read` there returns
/// `Err(Unsupported)`, so the native path does not crash, it just reports "no
/// vanilla pack found" and falls back to the demo palette. That fallback is
/// *visible* (it banners), which is why this seam is an addition rather than a
/// rescue.
///
/// The important observation is that **only the byte acquisition differs**.
/// `lodestone-assets`' `ResourceSource` is a synchronous, byte-based trait, and
/// `ZipSource::from_bytes` builds a fully in-memory pack; `lodestone-render`'s
/// `BlocksJsonRegistry::from_slice` is likewise ungated. So the browser crosses the
/// filesystem wall exactly once, asynchronously, at the byte source — and every
/// parser, atlas builder and model baker downstream runs unchanged. That is the
/// same shape `web/`'s earlier feasibility spike proved with a trimmed pack.
///
/// # How it works
///
/// `web/` `fetch`es the bytes before it starts the app, calls [`install`] once, and
/// `resources.rs` reads them back through [`bundle`]. It is a `OnceLock` rather than
/// a parameter threaded through `Config` because the consumers are ~20 lazily-called
/// `load_*` functions spread across `resources.rs`, each of which independently
/// re-resolves the pack root today; giving them a process-wide byte source is a
/// strictly smaller change than giving all of them a new argument, and it matches
/// what the native side already does (`SELECTED_PACKS` is a process-wide `RwLock`
/// for the same reason).
///
/// # How to change it
///
/// If you need a third asset blob, add a field — do **not** add a second
/// `OnceLock`, or the "were the assets installed?" question stops having one
/// answer. [`install`] deliberately reports whether it won the race instead of
/// silently ignoring a second call: two different bundles installed in one session
/// is a bug in the caller, and one that would otherwise present as "the textures
/// are from the wrong pack".
#[cfg(target_arch = "wasm32")]
pub mod assets {
    use std::sync::OnceLock;

    /// The asset blobs a browser session needs, as raw bytes.
    ///
    /// These are the two files `resources.rs`' native `try_vanilla` reads off
    /// disk, in the same roles: the jar is the `ResourceSource` (textures, models,
    /// blockstates, lang), and the report is the block-state id table the atlas and
    /// the model baker are built against.
    #[derive(Debug, Default)]
    pub struct Bundle {
        /// `client.jar` — the renderable corpus, consumed by `ZipSource::from_bytes`.
        pub client_jar: Vec<u8>,
        /// `generated/reports/blocks.json`, consumed by
        /// `BlocksJsonRegistry::from_slice`.
        pub blocks_report: Vec<u8>,
        /// The handful of `client.jar`-shadowed asset-object bytes `web/` could
        /// fetch and stage — today just whichever of the six real title-screen
        /// panorama faces `web/Trunk.toml`'s `post_build` hook resolved out of a
        /// local `.cache/mc/<version>` asset-object store, if any.
        ///
        /// Keyed by the same asset-index name
        /// `crate::menu::panorama::face_index_key` produces (no `assets/`
        /// prefix), so `crate::resources`'s wasm32 `load_panorama` arm can hand
        /// this straight to `crate::menu::panorama::load` as an
        /// [`crate::asset_objects::ObjectBytesSource`] with no translation.
        ///
        /// **Empty, not missing, is the expected default.** Native has a real
        /// `AssetObjectStore` to fall back to; a browser has neither that nor a
        /// filesystem, so an empty vec here is not an error — `panorama::load`
        /// already falls back to `client.jar`'s 1×1 grey stub per face, exactly
        /// as a native run with no populated store does. Any subset (not just
        /// all-six-or-nothing) is honoured: `web/Trunk.toml`'s hook stages
        /// per-face and reports which resolved.
        pub panorama: Vec<(String, Vec<u8>)>,
        /// `minecraft/sounds.json` — the event registry `crate::audio`'s wasm32
        /// `ShellAudio` parses with [`lodestone_assets::sound::SoundRegistry::parse`],
        /// the same file native's `AssetObjectStore` reads off disk. Small (~626 KB)
        /// relative to the corpus it indexes, so unlike the corpus itself it is a
        /// reasonable thing to fetch and stage whole.
        ///
        /// Empty is the honest "no store, no CDN reachable" default — `ShellAudio`
        /// degrades to `None` exactly as native does with no `sounds.json` on disk,
        /// never a silent stub.
        pub sounds_json: Vec<u8>,
        /// A curated subset of the `.ogg` corpus, staged the same way `panorama`
        /// is: keyed by the asset-index name (`minecraft/sounds/<path>.ogg`, no
        /// `assets/` prefix), so `crate::audio`'s wasm32 arm can feed this straight
        /// into a [`lodestone_assets::MemorySource`] after prepending `assets/` —
        /// the exact prefix [`lodestone_assets::sound::ResolvedSound::file_path`]
        /// already produces for the native lookup, so no second path scheme exists.
        ///
        /// **Never the full 4751-object / 80 MB corpus** — that is what would blow
        /// `web/`'s gzip ceiling (`just wasm-size`), not what this field is for.
        /// The intended source is a build-time staging step (mirroring
        /// `web/Trunk.toml`'s `post_build` panorama hook) that resolves a small,
        /// deliberately-chosen set out of a local asset-object store — block
        /// break/place/step, a handful of UI-adjacent and common mob events — and
        /// reports which of them resolved. Any subset is honoured; an event whose
        /// sample is not staged degrades exactly as a missing native object does
        /// (`ShellAudio::report_failure`, warn once then debug).
        pub sound_objects: Vec<(String, Vec<u8>)>,
    }

    static BUNDLE: OnceLock<Bundle> = OnceLock::new();

    /// Installs the session's asset bytes. Returns `Err` with the already-installed
    /// bundle's sizes if one was installed first.
    ///
    /// # Errors
    /// Returns `Err(String)` when a bundle is already installed — a caller bug,
    /// reported rather than swallowed, because the symptom of swallowing it is a
    /// world rendered from the wrong pack with nothing in the log.
    pub fn install(bundle: Bundle) -> Result<(), String> {
        let jar = bundle.client_jar.len();
        let report = bundle.blocks_report.len();
        BUNDLE.set(bundle).map_err(|_| {
            let live = BUNDLE.get().expect("set failed, so one is installed");
            format!(
                "asset bundle already installed ({} B jar, {} B report); \
                 refused to replace it with {jar} B / {report} B",
                live.client_jar.len(),
                live.blocks_report.len(),
            )
        })
    }

    /// The installed bytes, or `None` when `web/` has not called [`install`] yet.
    #[must_use]
    pub fn bundle() -> Option<&'static Bundle> {
        BUNDLE.get()
    }
}

/// The system clipboard, for `menu::edit_box::EditBox`'s copy/cut/paste
/// (`Minecraft.keyboardHandler.getClipboard`/`setClipboard`).
///
/// # Native vs. browser
///
/// A native OS clipboard call is genuinely synchronous — it returns the string
/// right there, in microseconds — which is what lets [`get`]/[`set`] be plain
/// functions and `EditBox::handle_key` stay a plain `fn(&mut self, KeyEvent) ->
/// bool` with no `async` anywhere in the menu stack.
///
/// A browser's `navigator.clipboard` is **not** synchronous: `readText`/
/// `writeText` both return a `Promise`, with `readText` additionally gated on a
/// user-activation/permission check that can itself prompt. [`set`] is still a
/// plain function on wasm32 — it fires the write and does not wait for it,
/// which is fine because vanilla's own `setClipboard` has no return value
/// either and nothing downstream needs to know when the browser finishes.
/// [`get`] cannot be given the same treatment without an `EditBox` that can
/// suspend, so on wasm32 it degrades to an **empty string**, synchronously —
/// the same "declined rather than faked" choice `EditBox`'s module docs made
/// for the whole clipboard before this seam existed, now scoped to just the
/// one platform that cannot honour a synchronous read. A paste that inserts
/// nothing is the honest degradation; inserting stale or wrong text would not
/// be.
#[cfg(not(target_arch = "wasm32"))]
pub mod clipboard {
    use std::sync::Mutex;

    // A single `arboard::Clipboard`, opened once and reused. On X11 in
    // particular, opening one per call is wasteful (it briefly owns the
    // selection via a background thread to answer a paste), and there is no
    // reason to pay that cost every keystroke.
    static HANDLE: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

    fn with_handle<T>(f: impl FnOnce(&mut arboard::Clipboard) -> T) -> Option<T> {
        let mut guard = HANDLE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_none() {
            *guard = arboard::Clipboard::new().ok();
        }
        guard.as_mut().map(f)
    }

    /// The clipboard's text contents, or an empty string if there is no
    /// clipboard, it holds no text, or the platform call fails — vanilla's own
    /// `getClipboard` swallows `UnsupportedFlavorException`/`IOException` the
    /// same way.
    #[must_use]
    pub fn get() -> String {
        with_handle(|cb| cb.get_text().ok()).flatten().unwrap_or_default()
    }

    /// Best-effort write. A failure here has no user-visible error path in
    /// vanilla either — `setClipboard` also just logs and moves on.
    pub fn set(text: &str) {
        let _ = with_handle(|cb| cb.set_text(text.to_owned()));
    }
}

/// See the native arm's docs for the sync/async split this mirrors.
#[cfg(target_arch = "wasm32")]
pub mod clipboard {
    /// Always empty — see the module doc's "why `get` degrades" note. Kept as
    /// a real function rather than deleted so `EditBox::handle_key`'s paste
    /// arm needs no `cfg` of its own.
    ///
    /// The decline is **logged**, once per session, rather than silent. A
    /// paste that inserts nothing is indistinguishable from a paste that
    /// inserted an empty clipboard, so without this line the only report
    /// anyone can make is "paste does nothing in the browser" with no way to
    /// tell a missing feature from a broken one. Once, not per keystroke:
    /// this is on the key path of every Cmd+V.
    #[must_use]
    pub fn get() -> String {
        use std::sync::atomic::{AtomicBool, Ordering};
        static WARNED: AtomicBool = AtomicBool::new(false);
        if !WARNED.swap(true, Ordering::Relaxed) {
            tracing::info!(
                "clipboard read declined on wasm32: `navigator.clipboard.readText` is \
                 async and permission-gated, and no text field here can suspend. Paste \
                 inserts nothing; copy and cut still write the real clipboard."
            );
        }
        String::new()
    }

    /// Fire-and-forget `navigator.clipboard.writeText`. Silently does nothing
    /// without a `Window` (there always is one in the browser build this
    /// compiles for) or without clipboard-write permission — the same
    /// "no visible error path" as vanilla's own `setClipboard`.
    pub fn set(text: &str) {
        let Some(window) = web_sys::window() else { return };
        let promise = window.navigator().clipboard().write_text(text);
        wasm_bindgen_futures::spawn_local(async move {
            let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
        });
    }
}

/// The relay's WebSocket URL, and a wasm32 sleep with no timer driver behind it.
///
/// # What it is
///
/// Two small primitives a browser-only relay ping needs and a native build does
/// not: where to dial the relay, and how to bound a wait for its answer.
///
/// # How it works
///
/// [`relay_ws_url`] used to be a second thing entirely — `web/src/main.rs`
/// hardcoded `ws://127.0.0.1:25580`, a literal a contributor had to remember to
/// change in lockstep with the justfile's `relay_defaults` `--listen` flag. That
/// constant is gone; `web/Trunk.toml` now proxies the fixed path `/relay` on the
/// page's own origin through to the relay's real listener (`[[proxies]]`, `ws =
/// true`), so the browser never needs a second port at all. [`relay_ws_url_from`]
/// is the pure half — scheme-and-host in, URL out — kept separate from
/// [`relay_ws_url`] specifically so it has a real native unit test; reading
/// `window.location` is not a unit worth testing, string formatting is.
///
/// **This only resolves to something with `trunk serve` running.** A deployed,
/// pre-built `dist/` has no trunk process behind it, so `/relay` 404s or refuses
/// the WebSocket upgrade outright — see `web/README.md`'s "Deployed builds" note.
/// The URL this returns is still well-formed there; there is simply nothing
/// listening on the far end unless the deployment's own reverse proxy adds an
/// equivalent forward.
///
/// [`sleep`] exists because `tokio::time::timeout` does not merely fail to fire
/// on `wasm32`, it hangs on its *first poll* — there is no entered runtime and so
/// no timer driver behind its deadline computation (measured while diagnosing an
/// unrelated wasm32 join stall; see `net.rs`'s `run_async` for the full writeup).
/// A relay ping still needs a real deadline — an unreachable relay must fail
/// visibly rather than leave a server-list row `Pending` forever — so this builds
/// one out of a JS `setTimeout`, which *is* backed by a real timer (the
/// browser's), and resolves the returned future from that callback. Race it
/// against the real work with `tokio::select!`, which needs no driver of its own
/// beyond polling whichever future's waker fires first.
///
/// # How to change it
///
/// If a second wasm-only caller needs a deadline, reuse [`sleep`] rather than a
/// second `set_timeout` callsite — `wasm_bindgen::closure::Closure` construction
/// is easy to get subtly wrong (see `ws_web.rs`'s comment on the `WebSocket`
/// `error` event for one way it already has).
pub mod relay {
    /// Builds a relay WebSocket URL from an origin's scheme and host, appending
    /// the fixed `/relay` path `web/Trunk.toml`'s proxy listens on.
    ///
    /// This is the relay **endpoint** only — where the browser's WebSocket
    /// connects — and it stays exactly this simple on purpose. The **destination**
    /// (which real Minecraft server the relay should bridge to) is per-connection
    /// data layered on top by [`relay_ws_url_for`], not a second thing this
    /// function needs to know; see that function's doc for why the split is
    /// deliberate and `lodestone_relay`'s crate doc for the full reasoning
    /// (`crates/lodestone-relay/src/lib.rs` — the relay used to dial one fixed
    /// `--target` for every connection, which is a bug this repo shipped and
    /// fixed: every server-list row showed the *same* backend's MOTD).
    ///
    /// **Deliberately not `cfg`-gated to `wasm32`, unlike the rest of this
    /// module** — it touches no `web_sys`, so it compiles and runs on every
    /// target, which is what gives it a real native `cargo test` (see the
    /// module doc: reading `window.location` is not a unit worth testing,
    /// string formatting is).
    #[must_use]
    pub fn relay_ws_url_from(https: bool, host: &str) -> String {
        let scheme = if https { "wss:" } else { "ws:" };
        format!("{scheme}//{host}/relay")
    }

    /// [`relay_ws_url_from`], reading the page's own scheme and host off
    /// `window.location`. Falls back to `127.0.0.1:8080` — `just run-wasm`'s
    /// default — if there is somehow no `Window` (there always is one in the
    /// browser build this compiles for).
    #[cfg(target_arch = "wasm32")]
    #[must_use]
    pub fn relay_ws_url() -> String {
        let Some(window) = web_sys::window() else {
            return relay_ws_url_from(false, "127.0.0.1:8080");
        };
        let location = window.location();
        let https = location.protocol().is_ok_and(|p| p == "https:");
        let host = location.host().unwrap_or_else(|_| "127.0.0.1:8080".to_string());
        relay_ws_url_from(https, &host)
    }

    /// Appends a per-connection `host`/`port` destination to a relay URL, as
    /// `?host=<percent-encoded>&port=<port>` — the query parameter
    /// `crates/lodestone-relay`'s `destination_from_query` reads off the
    /// WebSocket upgrade request before deciding where to dial.
    ///
    /// # Why the destination travels separately from [`relay_ws_url`]
    ///
    /// The relay endpoint is one fixed thing per page load (this origin's own
    /// `/relay`); the destination is a *choice the player made* — whichever
    /// server-list row or direct-connect address they picked — and can differ
    /// between two connections made moments apart (pinging one row while
    /// joining another). Folding it into [`relay_ws_url`] would make that
    /// function's "one relay, one definition" property a lie the moment two
    /// different destinations were needed in the same session. Every caller
    /// that dials the relay for a specific server — [`crate::menu::status`]'s
    /// `relay_probe` and `net.rs`'s browser join — calls this on top of
    /// [`relay_ws_url`], never a URL of its own construction.
    ///
    /// Pure string formatting, not `cfg`-gated, for the same testability reason
    /// as [`relay_ws_url_from`].
    #[must_use]
    pub fn with_destination(relay_url: &str, host: &str, port: u16) -> String {
        format!("{relay_url}?host={}&port={port}", percent_encode(host))
    }

    /// [`relay_ws_url`] plus [`with_destination`] — the URL every real
    /// per-server relay dial should use. Kept as one call so a caller cannot
    /// accidentally dial [`relay_ws_url`] bare and silently hit whatever the
    /// relay's own `--target` fallback (if any) resolves to instead of the
    /// server the player actually chose.
    #[cfg(target_arch = "wasm32")]
    #[must_use]
    pub fn relay_ws_url_for(host: &str, port: u16) -> String {
        with_destination(&relay_ws_url(), host, port)
    }

    /// Percent-encodes everything outside the URL query "unreserved" set
    /// (`ALPHA / DIGIT / "-" / "." / "_" / "~"`, RFC 3986 §2.3) so a hostname
    /// containing `&`, `=`, `%`, or non-ASCII bytes cannot be mistaken for query
    /// syntax or corrupt a neighbouring parameter. Real Minecraft server
    /// addresses are almost always plain ASCII hostnames or IP literals, for
    /// which this is a no-op — the encoding exists for the address a player
    /// *could* type, not the common case.
    fn percent_encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for byte in s.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    out.push(byte as char);
                }
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out
    }

    /// Resolves after `duration`, via a JS `setTimeout` rather than
    /// `tokio::time::timeout` — see the module doc for why the latter cannot be
    /// used here at all (it hangs, it does not merely fail to fire).
    #[cfg(target_arch = "wasm32")]
    pub async fn sleep(duration: std::time::Duration) {
        let millis = i32::try_from(duration.as_millis()).unwrap_or(i32::MAX);
        let Some(window) = web_sys::window() else {
            return;
        };
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            // A missed `set_timeout` call (the only failure mode here — an
            // exhausted timer-id space or similar) leaves `resolve` uncalled,
            // which means this future never completes rather than completing
            // early. That is the safe direction: a ping that never times out
            // degrades to "still pending", not to "reported success it never
            // had".
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
        });
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }

    #[cfg(test)]
    mod tests {
        use super::{relay_ws_url_from, with_destination};

        // Predicted values, not a round-trip through the function under test —
        // `decode(encode(x)) == x` would prove nothing about the scheme mapping.
        #[test]
        fn http_origin_maps_to_plain_ws() {
            assert_eq!(
                relay_ws_url_from(false, "127.0.0.1:8080"),
                "ws://127.0.0.1:8080/relay"
            );
        }

        #[test]
        fn https_origin_maps_to_wss() {
            assert_eq!(
                relay_ws_url_from(true, "example.com"),
                "wss://example.com/relay"
            );
        }

        #[test]
        fn host_carries_a_nonstandard_port_verbatim() {
            // The discriminating case: a fixture where scheme and host are both
            // pairwise-distinct from the other tests', so a transposed
            // scheme/host argument order would fail rather than coincidentally
            // pass.
            assert_eq!(
                relay_ws_url_from(false, "lodestone.example:9001"),
                "ws://lodestone.example:9001/relay"
            );
        }

        #[test]
        fn destination_appends_a_query_pair_with_no_percent_escaping_needed() {
            assert_eq!(
                with_destination("ws://127.0.0.1:8080/relay", "hypixel.net", 25565),
                "ws://127.0.0.1:8080/relay?host=hypixel.net&port=25565"
            );
        }

        #[test]
        fn two_different_destinations_on_the_same_relay_url_produce_two_different_urls() {
            // The exact discriminating property the relay-side bug lacked: this
            // client-side half must be capable of asking for two distinct
            // servers, not just capable of asking for *a* server.
            let base = "ws://127.0.0.1:8080/relay";
            let a = with_destination(base, "survival.example", 25565);
            let b = with_destination(base, "hypixel.net", 25565);
            assert_ne!(a, b, "two different hosts must produce two different URLs");
        }

        #[test]
        fn a_host_with_reserved_query_characters_is_percent_encoded() {
            // `&` and `=` are query syntax; an unescaped one would either start
            // a new (bogus) parameter or corrupt this one. `:` is not in the
            // unreserved set either (it is a `gen-delim`), which matters for a
            // bracketed IPv6 literal such as `[::1]`.
            assert_eq!(
                with_destination("ws://127.0.0.1:8080/relay", "a&b=c", 1),
                "ws://127.0.0.1:8080/relay?host=a%26b%3Dc&port=1"
            );
        }
    }
}
