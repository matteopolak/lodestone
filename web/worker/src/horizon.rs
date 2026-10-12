//! Worker side of the page's distant-terrain tile channel: the worker keeps the
//! world source and answers bounded tile requests with packed cell words.

use std::sync::Arc;

use lodestone_server::ChunkSource;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{MessageEvent, MessagePort};

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TileRequest {
    epoch: u32,
    request_id: u64,
    tile_x: i32,
    tile_z: i32,
}

pub(crate) struct HorizonService {
    port: MessagePort,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
}

impl HorizonService {
    pub(crate) fn new(source: Arc<dyn ChunkSource>, epoch: u32, port: MessagePort) -> Self {
        let response_port = port.clone();
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let Some(request) = parse_request(&event.data()) else {
                tracing::debug!("ignoring malformed browser horizon request");
                return;
            };
            if request.epoch != epoch {
                tracing::debug!("ignoring stale browser horizon request");
                return;
            }
            if let Err(error) = post_response(&response_port, &*source, request) {
                tracing::debug!(?error, "horizon tile response dropped");
            }
        });
        port.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        port.start();
        Self {
            port,
            _on_message: on_message,
        }
    }
}

impl Drop for HorizonService {
    fn drop(&mut self) {
        self.port.set_onmessage(None);
        self.port.close();
    }
}

fn wire_number(value: &JsValue, key: &str) -> Option<f64> {
    js_sys::Reflect::get(value, &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.as_f64())
        .filter(|value| value.is_finite() && value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER)
}

fn parse_request(value: &JsValue) -> Option<TileRequest> {
    let kind = js_sys::Reflect::get(value, &JsValue::from_str("kind"))
        .ok()
        .and_then(|value| value.as_string());
    (kind.as_deref() == Some("horizon-request")).then_some(())?;
    let epoch = u32::try_from(wire_number(value, "epoch")? as i64).ok()?;
    let request_id = u64::try_from(wire_number(value, "requestId")? as i64).ok()?;
    if epoch == 0 || request_id == 0 {
        return None;
    }
    Some(TileRequest {
        epoch,
        request_id,
        tile_x: i32::try_from(wire_number(value, "tileX")? as i64).ok()?,
        tile_z: i32::try_from(wire_number(value, "tileZ")? as i64).ok()?,
    })
}

/// A tile with any cell the source cannot sample is left unanswered; the page
/// keeps it pending and retries on recentre.
fn post_response(
    port: &MessagePort,
    source: &dyn ChunkSource,
    request: TileRequest,
) -> Result<(), JsValue> {
    let Some(words) =
        lodestone_server::horizon_tile::pack_horizon_tile(source, request.tile_x, request.tile_z)
    else {
        return Ok(());
    };
    let packed = js_sys::Uint32Array::from(words.as_slice());
    let message = js_sys::Object::new();
    let set = |key: &str, value: JsValue| js_sys::Reflect::set(&message, &JsValue::from_str(key), &value);
    set("kind", JsValue::from_str("horizon-response"))?;
    set("epoch", JsValue::from_f64(f64::from(request.epoch)))?;
    set("requestId", JsValue::from_f64(request.request_id as f64))?;
    set("tileX", JsValue::from_f64(f64::from(request.tile_x)))?;
    set("tileZ", JsValue::from_f64(f64::from(request.tile_z)))?;
    set("cells", packed.clone().into())?;
    let transfer = js_sys::Array::new();
    transfer.push(&packed.buffer());
    port.post_message_with_transferable(&message, &transfer)
}
