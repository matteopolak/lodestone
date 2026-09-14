//! Client-side ownership and bounded transport state for distant horizon data.

#[cfg(any(target_arch = "wasm32", test))]
use lodestone_render::{
    HORIZON_CELLS_PER_TILE, HORIZON_CELL_BLOCKS, HORIZON_TILE_CELLS, HORIZON_TILES_PER_AXIS,
};

use std::sync::Arc;

use lodestone_render::HorizonCell;

#[cfg(not(target_arch = "wasm32"))]
/// Native horizon sampler backed by the same source that owns the integrated
/// server world.
pub(crate) struct HorizonSurfaceQuery {
    source: Arc<dyn lodestone_server::ChunkSource>,
}

#[cfg(not(target_arch = "wasm32"))]
impl std::fmt::Debug for HorizonSurfaceQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HorizonSurfaceQuery").finish_non_exhaustive()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl HorizonSurfaceQuery {
    pub(crate) fn from_source(source: Arc<dyn lodestone_server::ChunkSource>) -> Option<Self> {
        source.horizon_sample(0, 0)?;
        Some(Self { source })
    }

    pub(crate) fn sample(&self, block_x: i32, block_z: i32) -> Option<HorizonCell> {
        self.source
            .horizon_sample(block_x, block_z)
            .map(horizon_cell)
    }
}

fn horizon_cell(sample: lodestone_server::HorizonSample) -> HorizonCell {
    let encode_y = |y: i32| y.saturating_add(64).clamp(0, i32::from(u16::MAX)) as u16;
    HorizonCell {
        terrain_y: encode_y(sample.terrain_y),
        water_y: sample.water_y.map_or(HorizonCell::DRY, encode_y),
        surface_rgb565: sample.surface_rgb565,
        flags: sample.flags,
    }
}

#[cfg(target_arch = "wasm32")]
use std::sync::Mutex;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::closure::Closure;

#[cfg(target_arch = "wasm32")]
use web_sys::{MessageEvent, MessagePort};

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HorizonTileRequest {
    pub(crate) epoch: u32,
    pub(crate) request_id: u64,
    pub(crate) tile_x: i32,
    pub(crate) tile_z: i32,
}

#[cfg(target_arch = "wasm32")]
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

#[cfg(any(target_arch = "wasm32", test))]
const MAX_WIRE_REQUEST_ID: u64 = 9_007_199_254_740_991;

#[cfg(target_arch = "wasm32")]
fn wire_number(value: &JsValue, key: &str) -> Option<f64> {
    js_sys::Reflect::get(value, &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.as_f64())
        .filter(|value| {
            value.is_finite() && value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER
        })
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn parse_tile_request(value: &JsValue) -> Option<HorizonTileRequest> {
    let kind = js_sys::Reflect::get(value, &JsValue::from_str("kind"))
        .ok()
        .and_then(|value| value.as_string());
    (kind.as_deref() == Some("horizon-request")).then_some(())?;
    let epoch = wire_number(value, "epoch")?.to_u32_checked()?;
    let request_id = wire_number(value, "requestId")?.to_u64_checked()?;
    if epoch == 0 || request_id == 0 {
        return None;
    }
    Some(HorizonTileRequest {
        epoch,
        request_id,
        tile_x: wire_number(value, "tileX")?.to_i32_checked()?,
        tile_z: wire_number(value, "tileZ")?.to_i32_checked()?,
    })
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingTile {
    coord: lodestone_render::HorizonTileCoord,
    request_id: u64,
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug)]
enum TileState {
    Pending(PendingTile),
    Ready(Box<[lodestone_render::HorizonCell]>),
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug)]
pub(crate) struct HorizonTileCache {
    epoch: u32,
    next_request_id: u64,
    entries: Vec<(lodestone_render::HorizonTileCoord, TileState)>,
    centre: lodestone_render::HorizonTileCoord,
}

#[cfg(any(target_arch = "wasm32", test))]
impl HorizonTileCache {
    fn new(epoch: u32) -> Self {
        Self {
            epoch,
            next_request_id: 1,
            entries: Vec::with_capacity(HORIZON_TILES_PER_AXIS * HORIZON_TILES_PER_AXIS),
            centre: lodestone_render::HorizonTileCoord { x: 0, z: 0 },
        }
    }

    fn recenter(&mut self, block_x: i32, block_z: i32) {
        let centre = lodestone_render::HorizonTileCoord::containing_block(block_x, block_z);
        if centre != self.centre {
            self.centre = centre;
            self.entries.clear();
        }
    }

    fn sample(
        &mut self,
        block_x: i32,
        block_z: i32,
    ) -> (Option<lodestone_render::HorizonCell>, Option<HorizonTileRequest>) {
        let coord = lodestone_render::HorizonTileCoord::containing_block(block_x, block_z);
        let (origin_x, origin_z) = coord.block_origin();
        let cell_x = block_x
            .saturating_sub(origin_x)
            .div_euclid(HORIZON_CELL_BLOCKS);
        let cell_z = block_z
            .saturating_sub(origin_z)
            .div_euclid(HORIZON_CELL_BLOCKS);
        if !(0..HORIZON_TILE_CELLS as i32).contains(&cell_x)
            || !(0..HORIZON_TILE_CELLS as i32).contains(&cell_z)
        {
            return (None, None);
        }
        let index = cell_z as usize * HORIZON_TILE_CELLS + cell_x as usize;
        if let Some((_, state)) = self.entries.iter().find(|(tile, _)| *tile == coord) {
            return match state {
                TileState::Pending(_) => (None, None),
                TileState::Ready(cells) => (cells.get(index).copied(), None),
            };
        }

        if self.entries.len() == HORIZON_TILES_PER_AXIS * HORIZON_TILES_PER_AXIS {
            self.entries.remove(0);
        }
        let request_id = self.next_request_id;
        self.next_request_id = if request_id == MAX_WIRE_REQUEST_ID {
            1
        } else {
            request_id + 1
        };
        self.entries.push((
            coord,
            TileState::Pending(PendingTile { coord, request_id }),
        ));
        (
            None,
            Some(HorizonTileRequest {
                epoch: self.epoch,
                request_id,
                tile_x: coord.x,
                tile_z: coord.z,
            }),
        )
    }

    fn complete(&mut self, response: HorizonTileResponse) -> bool {
        if response.epoch != self.epoch {
            return false;
        }
        if response.cells.len() != HORIZON_CELLS_PER_TILE {
            return false;
        }
        let Some((_, state)) = self
            .entries
            .iter_mut()
            .find(|(coord, _)| *coord == response.tile_coord)
        else {
            return false;
        };
        let TileState::Pending(pending) = state else {
            return false;
        };
        if pending.coord != response.tile_coord || pending.request_id != response.request_id {
            return false;
        }
        *state = TileState::Ready(response.cells);
        true
    }
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug)]
pub(crate) struct HorizonTileResponse {
    pub(crate) epoch: u32,
    pub(crate) request_id: u64,
    pub(crate) tile_coord: lodestone_render::HorizonTileCoord,
    pub(crate) cells: Box<[lodestone_render::HorizonCell]>,
}

#[cfg(target_arch = "wasm32")]
/// Worker-side horizon endpoint. The worker keeps the source and performs the
/// bounded tile query; the page receives only packed cell values.
pub(crate) struct HorizonWorkerService {
    port: MessagePort,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
}

#[cfg(target_arch = "wasm32")]
impl HorizonWorkerService {
    pub(crate) fn new(
        source: Arc<dyn lodestone_server::ChunkSource>,
        epoch: u32,
        port: MessagePort,
    ) -> Self {
        let response_port = port.clone();
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let Some(request) = parse_tile_request(&event.data()) else {
                tracing::debug!("ignoring malformed browser horizon request");
                return;
            };
            if request.epoch != epoch {
                tracing::debug!("ignoring stale browser horizon request");
                return;
            }
            if let Err(error) = post_tile_response(&response_port, &*source, request) {
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

#[cfg(target_arch = "wasm32")]
impl Drop for HorizonWorkerService {
    fn drop(&mut self) {
        self.port.set_onmessage(None);
        self.port.close();
    }
}

#[cfg(target_arch = "wasm32")]
fn post_tile_response(
    port: &MessagePort,
    source: &dyn lodestone_server::ChunkSource,
    request: HorizonTileRequest,
) -> Result<(), JsValue> {
    let coord = lodestone_render::HorizonTileCoord {
        x: request.tile_x,
        z: request.tile_z,
    };
    let (origin_x, origin_z) = coord.block_origin();
    let packed = js_sys::Uint32Array::new_with_length((HORIZON_CELLS_PER_TILE * 2) as u32);
    for z in 0..HORIZON_TILE_CELLS {
        for x in 0..HORIZON_TILE_CELLS {
            let block_x = origin_x.saturating_add((x as i32) * HORIZON_CELL_BLOCKS);
            let block_z = origin_z.saturating_add((z as i32) * HORIZON_CELL_BLOCKS);
            let Some(cell) = source.horizon_sample(block_x, block_z).map(horizon_cell) else {
                return Ok(());
            };
            let index = z * HORIZON_TILE_CELLS + x;
            packed.set_index(
                (index * 2) as u32,
                u32::from(cell.terrain_y) | (u32::from(cell.water_y) << 16),
            );
            packed.set_index(
                (index * 2 + 1) as u32,
                u32::from(cell.surface_rgb565) | (u32::from(cell.flags) << 16),
            );
        }
    }
    let message = js_sys::Object::new();
    let set = |key: &str, value: JsValue| {
        js_sys::Reflect::set(&message, &JsValue::from_str(key), &value)
    };
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

#[cfg(target_arch = "wasm32")]
/// Page-side horizon client. It stores at most one fixed renderer window of
/// tiles and asks the worker for a tile only on its first sample.
pub(crate) struct BrowserHorizonClient {
    cache: Arc<Mutex<HorizonTileCache>>,
    port: MessagePort,
}

#[cfg(target_arch = "wasm32")]
impl std::fmt::Debug for BrowserHorizonClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserHorizonClient").finish_non_exhaustive()
    }
}

#[cfg(target_arch = "wasm32")]
impl BrowserHorizonClient {
    pub(crate) fn new(epoch: u32, port: MessagePort) -> Self {
        Self {
            cache: Arc::new(Mutex::new(HorizonTileCache::new(epoch))),
            port,
        }
    }

    pub(crate) fn cache(&self) -> Arc<Mutex<HorizonTileCache>> {
        Arc::clone(&self.cache)
    }

    pub(crate) fn recenter(&self, block_x: i32, block_z: i32) {
        self.cache
            .lock()
            .expect("horizon cache lock poisoned")
            .recenter(block_x, block_z);
    }

    pub(crate) fn sample(
        &self,
        block_x: i32,
        block_z: i32,
    ) -> Option<lodestone_render::HorizonCell> {
        let (cell, request) = self
            .cache
            .lock()
            .expect("horizon cache lock poisoned")
            .sample(block_x, block_z);
        if let Some(request) = request {
            if let Err(error) = post_tile_request(&self.port, request) {
                tracing::debug!(?error, "horizon tile request dropped");
            }
        }
        cell
    }
}

#[cfg(target_arch = "wasm32")]
fn post_tile_request(port: &MessagePort, request: HorizonTileRequest) -> Result<(), JsValue> {
    let message = js_sys::Object::new();
    js_sys::Reflect::set(
        &message,
        &JsValue::from_str("kind"),
        &JsValue::from_str("horizon-request"),
    )?;
    js_sys::Reflect::set(
        &message,
        &JsValue::from_str("epoch"),
        &JsValue::from_f64(f64::from(request.epoch)),
    )?;
    js_sys::Reflect::set(
        &message,
        &JsValue::from_str("requestId"),
        &JsValue::from_f64(request.request_id as f64),
    )?;
    js_sys::Reflect::set(
        &message,
        &JsValue::from_str("tileX"),
        &JsValue::from_f64(f64::from(request.tile_x)),
    )?;
    js_sys::Reflect::set(
        &message,
        &JsValue::from_str("tileZ"),
        &JsValue::from_f64(f64::from(request.tile_z)),
    )?;
    port.post_message(&message)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn parse_tile_response(value: &JsValue) -> Option<HorizonTileResponse> {
    let string = |key: &str| {
        js_sys::Reflect::get(value, &JsValue::from_str(key))
            .ok()
            .and_then(|value| value.as_string())
    };
    if string("kind").as_deref() != Some("horizon-response") {
        return None;
    }
    let number = |key: &str| {
        js_sys::Reflect::get(value, &JsValue::from_str(key))
            .ok()
            .and_then(|value| value.as_f64())
            .filter(|value| {
                value.is_finite() && value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER
            })
    };
    let epoch = number("epoch")?.to_u32_checked()?;
    let request_id = number("requestId")?.to_u64_checked()?;
    if epoch == 0 || request_id == 0 {
        return None;
    }
    let tile_x = number("tileX")?.to_i32_checked()?;
    let tile_z = number("tileZ")?.to_i32_checked()?;
    let cells = js_sys::Reflect::get(value, &JsValue::from_str("cells"))
        .ok()?
        .dyn_into::<js_sys::Uint32Array>()
        .ok()?;
    if cells.length() as usize != HORIZON_CELLS_PER_TILE * 2 {
        return None;
    }
    let mut decoded = Vec::with_capacity(HORIZON_CELLS_PER_TILE);
    for index in 0..HORIZON_CELLS_PER_TILE {
        let height_water = cells.get_index((index * 2) as u32);
        let colour_flags = cells.get_index((index * 2 + 1) as u32);
        decoded.push(lodestone_render::HorizonCell {
            terrain_y: height_water as u16,
            water_y: (height_water >> 16) as u16,
            surface_rgb565: colour_flags as u16,
            flags: (colour_flags >> 16) as u16,
        });
    }
    Some(HorizonTileResponse {
        epoch,
        request_id,
        tile_coord: lodestone_render::HorizonTileCoord { x: tile_x, z: tile_z },
        cells: decoded.into_boxed_slice(),
    })
}

#[cfg(target_arch = "wasm32")]
trait HorizonWireNumber {
    fn to_u32_checked(self) -> Option<u32>;
    fn to_u64_checked(self) -> Option<u64>;
    fn to_i32_checked(self) -> Option<i32>;
}

#[cfg(target_arch = "wasm32")]
impl HorizonWireNumber for f64 {
    fn to_u32_checked(self) -> Option<u32> {
        (0.0..=f64::from(u32::MAX)).contains(&self).then_some(self as u32)
    }

    fn to_u64_checked(self) -> Option<u64> {
        (0.0..=9_007_199_254_740_991.0).contains(&self)
            .then_some(self as u64)
    }

    fn to_i32_checked(self) -> Option<i32> {
        (f64::from(i32::MIN)..=f64::from(i32::MAX))
            .contains(&self)
            .then_some(self as i32)
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn apply_tile_response(
    cache: &Arc<Mutex<HorizonTileCache>>,
    response: HorizonTileResponse,
) -> bool {
    let mut cache = cache.lock().expect("horizon cache lock poisoned");
    if cache.epoch != response.epoch {
        return false;
    }
    cache.complete(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(value: u16) -> Box<[lodestone_render::HorizonCell]> {
        vec![lodestone_render::HorizonCell {
            terrain_y: value,
            ..lodestone_render::HorizonCell::EMPTY
        }; HORIZON_CELLS_PER_TILE]
        .into_boxed_slice()
    }

    #[test]
    fn pending_tile_accepts_only_its_epoch_and_request() {
        let mut cache = HorizonTileCache::new(9);
        let (missing, request) = cache.sample(0, 0);
        assert!(missing.is_none());
        let request = request.expect("first cell must enqueue its tile");
        assert_eq!(request.epoch, 9);
        let (_, duplicate) = cache.sample(0, 0);
        assert!(duplicate.is_none(), "pending tiles must not be requested twice");

        let coord = lodestone_render::HorizonTileCoord { x: 0, z: 0 };
        assert!(!cache.complete(HorizonTileResponse {
            epoch: 8,
            request_id: request.request_id,
            tile_coord: coord,
            cells: cells(7),
        }));
        assert!(!cache.complete(HorizonTileResponse {
            epoch: 9,
            request_id: request.request_id + 1,
            tile_coord: coord,
            cells: cells(7),
        }));
        assert!(cache.complete(HorizonTileResponse {
            epoch: 9,
            request_id: request.request_id,
            tile_coord: coord,
            cells: cells(7),
        }));
        assert_eq!(cache.sample(0, 0).0.unwrap().terrain_y, 7);
    }

    #[test]
    fn cache_capacity_is_the_fixed_renderer_window() {
        let mut cache = HorizonTileCache::new(1);
        cache.recenter(1, 1);
        for index in 0..=(HORIZON_TILES_PER_AXIS * HORIZON_TILES_PER_AXIS) {
            let block = (index as i32) * HORIZON_TILE_CELLS as i32 * HORIZON_CELL_BLOCKS;
            let _ = cache.sample(block, 0);
        }
        assert_eq!(
            cache.entries.len(),
            HORIZON_TILES_PER_AXIS * HORIZON_TILES_PER_AXIS
        );
    }
}
