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
    #[derive(Debug)]
    pub struct Bundle {
        /// `client.jar` — the renderable corpus, consumed by `ZipSource::from_bytes`.
        pub client_jar: Vec<u8>,
        /// `generated/reports/blocks.json`, consumed by
        /// `BlocksJsonRegistry::from_slice`.
        pub blocks_report: Vec<u8>,
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
    /// same way (`KeyboardHandler.java`).
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
    #[must_use]
    pub fn get() -> String {
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
