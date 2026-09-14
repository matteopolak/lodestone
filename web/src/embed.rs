use std::{cell::{Cell, RefCell}, rc::Rc};

use js_sys::{Function, Object, Promise, Reflect, Uint8Array};
use lodestone::{CliOutcome, Config, Mode};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use web_sys::{HtmlCanvasElement, window};

#[wasm_bindgen]
pub struct LodestoneHandle {
    control: Option<lodestone::BrowserControl>,
    progress: Option<Rc<Function>>,
    active: Rc<Cell<bool>>,
    first_frame_raf: Rc<Cell<Option<i32>>>,
    mount_lease: Option<MountLease>,
    destroyed: bool,
}

impl Drop for LodestoneHandle {
    fn drop(&mut self) {
        self.active.set(false);
        cancel_first_frame(&self.first_frame_raf);
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
        .dyn_into::<HtmlCanvasElement>()
        .map_err(|_| JsValue::from_str("options.canvas must be an HTMLCanvasElement"))?;
    let provider = match property(&options, "assetProvider") {
        Some(value) => Some(
            value
                .dyn_into::<Function>()
                .map_err(|_| JsValue::from_str("options.assetProvider must be a function"))?,
        ),
        None => None,
    };
    let client_jar = required_asset(&options, provider.as_ref(), progress.as_ref(), "clientJar").await?;
    let blocks_report = required_asset(&options, provider.as_ref(), progress.as_ref(), "blocksJson").await?;
    mount_bundle(
        canvas,
        lodestone::platform::assets::Bundle {
            client_jar,
            blocks_report,
            panorama: Vec::new(),
            sounds_json: Vec::new(),
            sound_objects: Vec::new(),
        },
        progress,
    )
    .await
}

pub(crate) async fn mount_bundle(
    canvas: HtmlCanvasElement,
    bundle: lodestone::platform::assets::Bundle,
    progress: Option<Rc<Function>>,
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
    let control = lodestone::run_browser(config, canvas.clone(), lease.lifecycle())
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    lease.mark_started();
    let first_frame_raf = Rc::new(Cell::new(None));
    let handle = LodestoneHandle {
        control: Some(control),
        progress,
        active: Rc::new(Cell::new(true)),
        first_frame_raf: Rc::clone(&first_frame_raf),
        mount_lease: Some(lease),
        destroyed: false,
    };
    handle.emit("started", 0.9, "renderer scheduled");
    schedule_first_frame(
        handle.progress.clone(),
        handle.active.clone(),
        first_frame_raf,
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
    raf_slot: Rc<Cell<Option<i32>>>,
) {
    poll_first_frame(progress, active, raf_slot, 0);
}

fn poll_first_frame(
    progress: Option<Rc<Function>>,
    active: Rc<Cell<bool>>,
    raf_slot: Rc<Cell<Option<i32>>>,
    attempt: u16,
) {
    let Some(window) = window() else {
        return;
    };
    let Some(listener) = progress else {
        return;
    };
    let callback_raf_slot = Rc::clone(&raf_slot);
    let closure = Closure::once(move || {
        if !active.get() {
            return;
        }
        callback_raf_slot.set(None);
        if lodestone::browser_first_frame_submitted() {
            emit_callback(&listener, "first-frame", 1.0, "first frame submitted");
        } else if attempt < 240 {
            poll_first_frame(Some(listener), active, callback_raf_slot, attempt + 1);
        } else {
            emit_callback(&listener, "first-frame-timeout", 0.0, "renderer did not submit a frame");
        }
    });
    if let Ok(id) = window.request_animation_frame(closure.as_ref().unchecked_ref()) {
        raf_slot.set(Some(id));
        closure.forget();
    }
}

fn cancel_first_frame(raf_slot: &Rc<Cell<Option<i32>>>) {
    if let Some(id) = raf_slot.take() {
        if let Some(window) = window() {
            let _ = window.cancel_animation_frame(id);
        }
    }
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
    let Some(window) = window() else {
        return;
    };
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
    if window
        .request_animation_frame(closure.as_ref().unchecked_ref())
        .is_ok()
    {
        closure.forget();
    }
}

async fn next_animation_frame() -> Result<(), JsValue> {
    let promise = Promise::new(&mut |resolve, reject| {
        let Some(window) = window() else {
            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str("no browser window"));
            return;
        };
        let closure = Closure::once_into_js(move || {
            let _ = resolve.call0(&JsValue::NULL);
        });
        if let Err(error) = window.request_animation_frame(closure.unchecked_ref()) {
            let _ = reject.call1(&JsValue::NULL, &error);
        }
    });
    JsFuture::from(promise).await.map(|_| ())
}
