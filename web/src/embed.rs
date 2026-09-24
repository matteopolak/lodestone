use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use js_sys::{Function, Object, Promise, Reflect, Uint8Array};
use lodestone::{CliOutcome, Config, Mode};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use web_sys::{OffscreenCanvas, window};

#[wasm_bindgen]
pub struct LodestoneHandle {
    control: Option<Rc<lodestone::BrowserControl>>,
    progress: Option<Rc<Function>>,
    host_action: Option<Rc<Function>>,
    active: Rc<Cell<bool>>,
    first_frame_raf: Rc<Cell<Option<i32>>>,
    host_action_timer: Rc<Cell<Option<i32>>>,
    join_progress_timer: Rc<Cell<Option<i32>>>,
    mount_lease: Option<MountLease>,
    destroyed: bool,
}

impl Drop for LodestoneHandle {
    fn drop(&mut self) {
        self.active.set(false);
        cancel_first_frame(&self.first_frame_raf);
        cancel_host_actions(&self.host_action_timer);
        cancel_host_actions(&self.join_progress_timer);
        if let Some(control) = self.control.take() {
            let _ = control.shutdown();
        }
        if let Some(mut lease) = self.mount_lease.take() {
            lease.schedule_completion(None);
        }
    }
}

#[wasm_bindgen]
impl LodestoneHandle {
    pub fn destroy(&mut self) -> Result<(), JsValue> {
        if self.destroyed {
            return Ok(());
        }
        self.destroyed = true;
        self.active.set(false);
        cancel_first_frame(&self.first_frame_raf);
        cancel_host_actions(&self.host_action_timer);
        cancel_host_actions(&self.join_progress_timer);
        let shutdown = self
            .control
            .take()
            .map_or(Ok(()), |control| control.shutdown())
            .map_err(|error| JsValue::from_str(&error));
        if let Some(mut lease) = self.mount_lease.take() {
            lease.schedule_completion(self.progress.clone());
        }
        shutdown
    }

    #[wasm_bindgen(js_name = isDestroyed)]
    pub fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    fn input_control(&self) -> Result<&lodestone::BrowserControl, JsValue> {
        self.control
            .as_deref()
            .ok_or_else(|| JsValue::from_str("Lodestone session is destroyed"))
    }

    #[wasm_bindgen(js_name = pointerMove)]
    pub fn pointer_move(&self, x: f64, y: f64) -> Result<(), JsValue> {
        self.input_control()?.browser_pointer_move(x, y);
        Ok(())
    }

    #[wasm_bindgen(js_name = mouseMotion)]
    pub fn mouse_motion(&self, dx: f64, dy: f64) -> Result<(), JsValue> {
        self.input_control()?.browser_mouse_motion(dx, dy);
        Ok(())
    }

    #[wasm_bindgen(js_name = mouseButton)]
    pub fn mouse_button(&self, button: u16, pressed: bool) -> Result<(), JsValue> {
        self.input_control()?
            .browser_mouse_button(button, pressed)
            .map_err(|error| JsValue::from_str(&error))
    }

    pub fn wheel(&self, dx: f64, dy: f64) -> Result<(), JsValue> {
        self.input_control()?.browser_wheel(dx, dy);
        Ok(())
    }

    pub fn focus(&self, focused: bool) -> Result<(), JsValue> {
        self.input_control()?.browser_focus(focused);
        Ok(())
    }

    pub fn resize(&self, width: u32, height: u32) -> Result<(), JsValue> {
        self.input_control()?.browser_resize(width, height);
        Ok(())
    }

    #[wasm_bindgen(js_name = pointerLock)]
    pub fn pointer_lock(&self, locked: bool) -> Result<(), JsValue> {
        self.input_control()?.browser_pointer_lock(locked);
        Ok(())
    }

    pub fn key(
        &self,
        code: String,
        pressed: bool,
        text: Option<String>,
        modifiers: u8,
    ) -> Result<(), JsValue> {
        self.input_control()?
            .browser_key(&code, pressed, text, modifiers)
            .map_err(|error| JsValue::from_str(&error))
    }

    fn emit(&self, kind: &str, fraction: f64, message: &str) {
        let Some(callback) = self.progress.as_ref() else {
            return;
        };
        let event = Object::new();
        let _ = Reflect::set(&event, &JsValue::from_str("type"), &JsValue::from_str(kind));
        let _ = Reflect::set(&event, &JsValue::from_str("phase"), &JsValue::from_str(kind));
        let _ = Reflect::set(
            &event,
            &JsValue::from_str("fraction"),
            &JsValue::from_f64(fraction.clamp(0.0, 1.0)),
        );
        let _ = Reflect::set(
            &event,
            &JsValue::from_str("message"),
            &JsValue::from_str(message),
        );
        let _ = callback.call1(&JsValue::NULL, &event);
    }
}

#[wasm_bindgen]
pub async fn mount(options: JsValue) -> Result<LodestoneHandle, JsValue> {
    let log_level = browser_log_level(property(&options, "logLevel"))?;
    log::set_max_level(log_level);
    log::info!("browser diagnostics enabled at {log_level}");
    let progress = match property(&options, "onProgress") {
        Some(value) => Some(Rc::new(
            value
                .dyn_into::<Function>()
                .map_err(|_| JsValue::from_str("options.onProgress must be a function"))?,
        )),
        None => None,
    };
    let canvas = property(&options, "canvas")
        .ok_or_else(|| JsValue::from_str("mount requires options.canvas"))?
        .dyn_into::<OffscreenCanvas>()
        .map_err(|_| JsValue::from_str("options.canvas must be an OffscreenCanvas"))?;
    let provider = match property(&options, "assetProvider") {
        Some(value) => Some(
            value
                .dyn_into::<Function>()
                .map_err(|_| JsValue::from_str("options.assetProvider must be a function"))?,
        ),
        None => None,
    };
    let host_action = match property(&options, "onHostAction") {
        Some(value) => Some(Rc::new(
            value
                .dyn_into::<Function>()
                .map_err(|_| JsValue::from_str("options.onHostAction must be a function"))?,
        )),
        None => None,
    };
    let client_jar = required_asset(&options, provider.as_ref(), progress.as_ref(), "clientJar").await?;
    let blocks_report = required_asset(&options, provider.as_ref(), progress.as_ref(), "blocksJson").await?;
    let panorama = panorama_assets(&options, provider.as_ref(), progress.as_ref()).await?;
    mount_bundle(
        canvas,
        lodestone::platform::assets::Bundle {
            client_jar,
            blocks_report,
            panorama,
            sounds_json: Vec::new(),
            sound_objects: Vec::new(),
        },
        progress,
        host_action,
    )
    .await
}

fn browser_log_level(value: Option<JsValue>) -> Result<log::LevelFilter, JsValue> {
    match value.and_then(|value| value.as_string()).as_deref() {
        None | Some("warn") => Ok(log::LevelFilter::Warn),
        Some("off") => Ok(log::LevelFilter::Off),
        Some("error") => Ok(log::LevelFilter::Error),
        Some("info") => Ok(log::LevelFilter::Info),
        Some("debug") => Ok(log::LevelFilter::Debug),
        Some("trace") => Ok(log::LevelFilter::Trace),
        Some(_) => Err(JsValue::from_str(
            "options.logLevel must be off, error, warn, info, debug, or trace",
        )),
    }
}

async fn panorama_assets(
    options: &JsValue,
    provider: Option<&Function>,
    progress: Option<&Rc<Function>>,
) -> Result<Vec<(String, Vec<u8>)>, JsValue> {
    let Some(provider) = provider else {
        return Ok(Vec::new());
    };
    let mut panorama = Vec::with_capacity(6);
    for index in 0..6 {
        let name = format!("panorama_{index}.png");
        let bytes = required_asset(options, Some(provider), progress, &name).await?;
        panorama.push((
            format!("minecraft/textures/gui/title/background/{name}"),
            bytes,
        ));
    }
    Ok(panorama)
}

pub(crate) async fn mount_bundle(
    canvas: OffscreenCanvas,
    bundle: lodestone::platform::assets::Bundle,
    progress: Option<Rc<Function>>,
    host_action: Option<Rc<Function>>,
) -> Result<LodestoneHandle, JsValue> {
    let mut lease = MountLease::claim().await?;
    install_bundle(bundle)?;

    let config = match Config::from_args(std::iter::empty::<String>()) {
        CliOutcome::Run(mut config) => {
            config.mode = Mode::Window;
            config.resolve_persisted(&lodestone::config::Options::load());
            config
        }
        CliOutcome::Help(text) => return Err(JsValue::from_str(&text)),
        CliOutcome::Error(error) => return Err(JsValue::from_str(&error)),
    };
    if let Some(callback) = progress.as_ref() {
        emit_callback(callback, "starting", 0.85, "starting Lodestone");
    }
    let control = Rc::new(
        lodestone::run_browser_offscreen(config, canvas, lease.lifecycle())
            .map_err(|error| JsValue::from_str(&error.to_string()))?,
    );
    let action_control = Rc::clone(&control);
    let first_frame_signal = control.first_frame_signal();
    lease.mark_started();
    let first_frame_raf = Rc::new(Cell::new(None));
    let host_action_timer = Rc::new(Cell::new(None));
    let join_progress_timer = Rc::new(Cell::new(None));
    let handle = LodestoneHandle {
        control: Some(control),
        progress,
        host_action,
        active: Rc::new(Cell::new(true)),
        first_frame_raf: Rc::clone(&first_frame_raf),
        host_action_timer: Rc::clone(&host_action_timer),
        join_progress_timer: Rc::clone(&join_progress_timer),
        mount_lease: Some(lease),
        destroyed: false,
    };
    handle.emit("started", 0.9, "renderer scheduled");
    schedule_first_frame(
        handle.progress.clone(),
        handle.active.clone(),
        first_frame_signal,
        first_frame_raf,
    );
    schedule_host_actions(
        handle.host_action.clone(),
        handle.active.clone(),
        action_control,
        host_action_timer,
    );
    schedule_join_progress(
        handle.progress.clone(),
        handle.active.clone(),
        Rc::clone(handle.control.as_ref().expect("mounted control exists")),
        join_progress_timer,
    );
    Ok(handle)
}

fn install_bundle(bundle: lodestone::platform::assets::Bundle) -> Result<(), JsValue> {
    if let Some(installed) = lodestone::platform::assets::bundle() {
        if bundles_match(installed, &bundle) {
            return Ok(());
        }
        return Err(JsValue::from_str(
            "a different asset bundle is already installed; use a fresh Wasm module for it",
        ));
    }
    lodestone::platform::assets::install(bundle).map_err(|error| JsValue::from_str(&error))
}

fn bundles_match(
    installed: &lodestone::platform::assets::Bundle,
    requested: &lodestone::platform::assets::Bundle,
) -> bool {
    installed.client_jar == requested.client_jar
        && installed.blocks_report == requested.blocks_report
        && installed.panorama == requested.panorama
        && installed.sounds_json == requested.sounds_json
        && installed.sound_objects == requested.sound_objects
}

fn schedule_first_frame(
    progress: Option<Rc<Function>>,
    active: Rc<Cell<bool>>,
    frame_signal: lodestone::BrowserFrameSignal,
    raf_slot: Rc<Cell<Option<i32>>>,
) {
    poll_first_frame(progress, active, frame_signal, raf_slot, 0);
}

fn poll_first_frame(
    progress: Option<Rc<Function>>,
    active: Rc<Cell<bool>>,
    frame_signal: lodestone::BrowserFrameSignal,
    raf_slot: Rc<Cell<Option<i32>>>,
    attempt: u16,
) {
    let Some(listener) = progress else {
        return;
    };
    let callback_raf_slot = Rc::clone(&raf_slot);
    let closure = Closure::once(move || {
        if !active.get() {
            return;
        }
        callback_raf_slot.set(None);
        if frame_signal.is_set() {
            emit_callback(&listener, "first-frame", 1.0, "first frame submitted");
        } else if attempt < 240 {
            poll_first_frame(
                Some(listener),
                active,
                frame_signal,
                callback_raf_slot,
                attempt + 1,
            );
        } else {
            emit_callback(&listener, "first-frame-timeout", 0.0, "renderer did not submit a frame");
        }
    });
    let scheduled = window()
        .map(|window| window.request_animation_frame(closure.as_ref().unchecked_ref()))
        .unwrap_or_else(|| request_timeout(closure.as_ref(), 16.0));
    if let Ok(id) = scheduled {
        raf_slot.set(Some(id));
        closure.forget();
    }
}

fn request_timeout(callback: &JsValue, milliseconds: f64) -> Result<i32, JsValue> {
    let global = js_sys::global();
    let timeout = Reflect::get(&global, &JsValue::from_str("setTimeout"))?.dyn_into::<Function>()?;
    timeout
        .call2(
            &global,
            callback,
            &JsValue::from_f64(milliseconds.max(0.0)),
        )?
        .as_f64()
        .map(|id| id as i32)
        .ok_or_else(|| JsValue::from_str("setTimeout returned a non-numeric id"))
}

fn cancel_first_frame(raf_slot: &Rc<Cell<Option<i32>>>) {
    if let Some(id) = raf_slot.take() {
        if let Some(window) = window() {
            let _ = window.cancel_animation_frame(id);
        } else if let Ok(clear_timeout) = Reflect::get(
            &js_sys::global(),
            &JsValue::from_str("clearTimeout"),
        )
        .and_then(|value| value.dyn_into::<Function>())
        {
            let _ = clear_timeout.call1(
                &js_sys::global(),
                &JsValue::from_f64(f64::from(id)),
            );
        }
    }
}

fn cancel_host_actions(timer_slot: &Rc<Cell<Option<i32>>>) {
    let Some(id) = timer_slot.take() else {
        return;
    };
    if let Some(window) = window() {
        let _ = window.cancel_animation_frame(id);
    } else if let Ok(clear_timeout) = Reflect::get(
        &js_sys::global(),
        &JsValue::from_str("clearTimeout"),
    )
    .and_then(|value| value.dyn_into::<Function>())
    {
        let _ = clear_timeout.call1(&js_sys::global(), &JsValue::from_f64(f64::from(id)));
    }
}

fn schedule_host_actions(
    callback: Option<Rc<Function>>,
    active: Rc<Cell<bool>>,
    control: Rc<lodestone::BrowserControl>,
    timer_slot: Rc<Cell<Option<i32>>>,
) {
    let Some(callback) = callback else {
        return;
    };
    let callback_timer = Rc::clone(&timer_slot);
    let next_timer = Rc::clone(&timer_slot);
    let closure = Closure::once(move || {
        callback_timer.set(None);
        if !active.get() {
            return;
        }
        if let Some(locked) = control.take_browser_pointer_lock_action() {
            let action = Object::new();
            let _ = Reflect::set(
                &action,
                &JsValue::from_str("type"),
                &JsValue::from_str("pointer-lock"),
            );
            let _ = Reflect::set(
                &action,
                &JsValue::from_str("locked"),
                &JsValue::from_bool(locked),
            );
            let _ = callback.call1(&JsValue::NULL, &action);
        }
        schedule_host_actions(Some(callback), active, control, next_timer);
    });
    if let Ok(id) = request_timeout(closure.as_ref(), 16.0) {
        timer_slot.set(Some(id));
        closure.forget();
    }
}

fn schedule_join_progress(
    callback: Option<Rc<Function>>,
    active: Rc<Cell<bool>>,
    control: Rc<lodestone::BrowserControl>,
    timer_slot: Rc<Cell<Option<i32>>>,
) {
    let Some(callback) = callback else {
        return;
    };
    let callback_timer = Rc::clone(&timer_slot);
    let next_timer = Rc::clone(&timer_slot);
    let closure = Closure::once(move || {
        callback_timer.set(None);
        if !active.get() {
            return;
        }
        for progress in control.take_join_progress() {
            emit_join_progress(&callback, &progress);
        }
        schedule_join_progress(Some(callback), active, control, next_timer);
    });
    if let Ok(id) = request_timeout(closure.as_ref(), 50.0) {
        timer_slot.set(Some(id));
        closure.forget();
    }
}

fn emit_join_progress(callback: &Function, progress: &lodestone::BrowserJoinProgress) {
    let event = Object::new();
    let expected = progress.expected_columns;
    let fraction = if expected == 0 {
        0.0
    } else {
        progress.loaded_columns as f64 / expected as f64
    };
    let message = match progress.phase {
        "world-create-started" => "creating world",
        "world-open-started" => "opening world",
        "joining" => "joining integrated server",
        "loading-terrain" => "loading terrain",
        "loading-overlay-ready" => "loading overlay ready to dismiss",
        "first-terrain-presented" => "first terrain frame presented",
        "full-view-presented" => "configured view presented",
        _ => "singleplayer join progress",
    };
    let fields = [
        ("type", JsValue::from_str(progress.phase)),
        ("phase", JsValue::from_str(progress.phase)),
        ("fraction", JsValue::from_f64(fraction.clamp(0.0, 1.0))),
        ("message", JsValue::from_str(message)),
        ("elapsedMs", JsValue::from_f64(progress.elapsed_ms)),
        ("loadedColumns", JsValue::from_f64(progress.loaded_columns as f64)),
        ("expectedColumns", JsValue::from_f64(expected as f64)),
        ("settledColumns", JsValue::from_f64(progress.settled_columns as f64)),
        ("pendingMeshes", JsValue::from_f64(progress.pending_meshes as f64)),
    ];
    for (name, value) in fields {
        let _ = Reflect::set(&event, &JsValue::from_str(name), &value);
    }
    let _ = callback.call1(&JsValue::NULL, &event);
}

fn emit_callback(callback: &Function, kind: &str, fraction: f64, message: &str) {
    emit_callback_with_asset(callback, kind, fraction, message, None, None, None);
}

fn emit_callback_with_asset(
    callback: &Function,
    kind: &str,
    fraction: f64,
    message: &str,
    asset_name: Option<&str>,
    loaded_bytes: Option<usize>,
    total_bytes: Option<usize>,
) {
    let event = Object::new();
    let _ = Reflect::set(&event, &JsValue::from_str("type"), &JsValue::from_str(kind));
    let _ = Reflect::set(
        &event,
        &JsValue::from_str("phase"),
        &JsValue::from_str(kind),
    );
    let _ = Reflect::set(
        &event,
        &JsValue::from_str("fraction"),
        &JsValue::from_f64(fraction),
    );
    let _ = Reflect::set(
        &event,
        &JsValue::from_str("message"),
        &JsValue::from_str(message),
    );
    if let Some(asset_name) = asset_name {
        let _ = Reflect::set(
            &event,
            &JsValue::from_str("assetName"),
            &JsValue::from_str(asset_name),
        );
    }
    if let Some(loaded_bytes) = loaded_bytes {
        let _ = Reflect::set(
            &event,
            &JsValue::from_str("loadedBytes"),
            &JsValue::from_f64(loaded_bytes as f64),
        );
    }
    if let Some(total_bytes) = total_bytes {
        let _ = Reflect::set(
            &event,
            &JsValue::from_str("totalBytes"),
            &JsValue::from_f64(total_bytes as f64),
        );
    }
    let _ = callback.call1(&JsValue::NULL, &event);
}

fn property(value: &JsValue, name: &str) -> Option<JsValue> {
    Reflect::get(value, &JsValue::from_str(name)).ok().filter(|value| !value.is_undefined() && !value.is_null())
}

async fn required_asset(
    options: &JsValue,
    provider: Option<&Function>,
    progress: Option<&Rc<Function>>,
    name: &str,
) -> Result<Vec<u8>, JsValue> {
    let direct = property(options, name);
    if let Some(callback) = progress {
        let total_bytes = direct.as_ref().and_then(byte_length);
        emit_callback_with_asset(
            callback,
            "asset-start",
            0.0,
            &format!("loading {name}"),
            Some(name),
            Some(0),
            total_bytes,
        );
    }
    let value = match direct.clone() {
        Some(value) => value,
        None => {
            let Some(provider) = provider else {
                emit_asset_error(progress, name);
                return Err(JsValue::from_str(&format!(
                    "mount requires options.{name} or options.assetProvider"
                )));
            };
            let result = match provider.call1(&JsValue::NULL, &JsValue::from_str(name)) {
                Ok(result) => result,
                Err(error) => {
                    emit_asset_error(progress, name);
                    return Err(JsValue::from_str(&format!(
                        "assetProvider({name}) failed: {error:?}"
                    )));
                }
            };
            match JsFuture::from(Promise::resolve(&result)).await {
                Ok(value) => value,
                Err(error) => {
                    emit_asset_error(progress, name);
                    return Err(JsValue::from_str(&format!(
                        "assetProvider({name}) rejected: {error:?}"
                    )));
                }
            }
        }
    };
    let bytes = match bytes_value(value, name) {
        Ok(bytes) => bytes,
        Err(error) => {
            emit_asset_error(progress, name);
            return Err(error);
        }
    };
    if let Some(callback) = progress {
        emit_callback_with_asset(
            callback,
            "asset-ready",
            1.0,
            &format!("loaded {name}"),
            Some(name),
            Some(bytes.len()),
            direct.as_ref().and_then(byte_length),
        );
    }
    Ok(bytes)
}

fn emit_asset_error(progress: Option<&Rc<Function>>, name: &str) {
    if let Some(progress) = progress {
        emit_callback_with_asset(
            progress,
            "asset-error",
            0.0,
            &format!("failed to load {name}"),
            Some(name),
            None,
            None,
        );
    }
}

fn byte_length(value: &JsValue) -> Option<usize> {
    if let Ok(bytes) = value.clone().dyn_into::<Uint8Array>() {
        return Some(bytes.length() as usize);
    }
    value
        .clone()
        .dyn_into::<js_sys::ArrayBuffer>()
        .ok()
        .map(|buffer| buffer.byte_length() as usize)
}

fn bytes_value(value: JsValue, name: &str) -> Result<Vec<u8>, JsValue> {
    if let Ok(bytes) = value.clone().dyn_into::<Uint8Array>() {
        return Ok(bytes.to_vec());
    }
    if value.is_instance_of::<js_sys::ArrayBuffer>() {
        return Ok(Uint8Array::new(&value).to_vec());
    }
    Err(JsValue::from_str(&format!("asset {name} must be Uint8Array or ArrayBuffer")))
}

struct MountLease {
    id: u64,
    lifecycle: Rc<Cell<bool>>,
    started: bool,
}

struct MountRecord {
    id: u64,
    shutting_down: bool,
}

thread_local! {
    static ACTIVE_MOUNT: RefCell<Option<MountRecord>> = const { RefCell::new(None) };
    static NEXT_MOUNT_ID: Cell<u64> = const { Cell::new(0) };
}

impl MountLease {
    async fn claim() -> Result<Self, JsValue> {
        loop {
            let occupied = ACTIVE_MOUNT.with(|slot| {
                slot.borrow()
                    .as_ref()
                    .map(|record| record.shutting_down)
            });
            match occupied {
                Some(false) => {
                    return Err(JsValue::from_str("a Lodestone session is already mounted"));
                }
                Some(true) => next_animation_frame().await?,
                None => break,
            }
        }

        let lifecycle = Rc::new(Cell::new(false));
        ACTIVE_MOUNT.with(|slot| {
            let mut slot = slot.borrow_mut();
            let id = NEXT_MOUNT_ID.with(|next| {
                let id = next.get().wrapping_add(1);
                next.set(id);
                id
            });
            *slot = Some(MountRecord {
                id,
                shutting_down: false,
            });
            Ok(Self {
                id,
                lifecycle,
                started: false,
            })
        })
    }

    fn lifecycle(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.lifecycle)
    }

    fn mark_started(&mut self) {
        self.started = true;
    }

    fn schedule_completion(&mut self, progress: Option<Rc<Function>>) {
        ACTIVE_MOUNT.with(|slot| {
            if let Some(record) = slot.borrow_mut().as_mut()
                && record.id == self.id
            {
                record.shutting_down = true;
            }
        });
        poll_mount_completion(
            self.id,
            Rc::clone(&self.lifecycle),
            progress,
        );
        self.started = false;
    }
}

impl Drop for MountLease {
    fn drop(&mut self) {
        ACTIVE_MOUNT.with(|slot| {
            if slot.borrow().as_ref().is_some_and(|record| {
                record.id == self.id && !self.started && !record.shutting_down
            }) {
                slot.replace(None);
            }
        });
    }
}

fn poll_mount_completion(
    id: u64,
    lifecycle: Rc<Cell<bool>>,
    progress: Option<Rc<Function>>,
) {
    let closure = Closure::once(move || {
        if !lifecycle.get() {
            poll_mount_completion(id, lifecycle, progress);
            return;
        }
        ACTIVE_MOUNT.with(|slot| {
            if slot.borrow().as_ref().is_some_and(|record| record.id == id) {
                slot.replace(None);
            }
        });
        if let Some(progress) = progress {
            emit_callback(&progress, "destroyed", 1.0, "session stopped");
        }
    });
    if window()
        .map(|window| window.request_animation_frame(closure.as_ref().unchecked_ref()))
        .unwrap_or_else(|| request_timeout(closure.as_ref(), 16.0))
        .is_ok()
    {
        closure.forget();
    }
}

async fn next_animation_frame() -> Result<(), JsValue> {
    let promise = Promise::new(&mut |resolve, reject| {
        let closure = Closure::once_into_js(move || {
            let _ = resolve.call0(&JsValue::NULL);
        });
        let scheduled = window()
            .map(|window| window.request_animation_frame(closure.unchecked_ref()))
            .unwrap_or_else(|| request_timeout(closure.as_ref(), 16.0));
        if let Err(error) = scheduled {
            let _ = reject.call1(&JsValue::NULL, &error);
        }
    });
    JsFuture::from(promise).await.map(|_| ())
}
