//! Uncapped rendering decoupled from a compositor-paced swapchain.
//!
//! A windowed Metal layer recycles drawables only as the compositor consumes
//! them, so with VSync off a renderer that acquires a drawable per frame still
//! blocks at the display refresh rate. In mailbox mode frames render into a
//! ring of three offscreen textures instead. A presenter thread blocks on the
//! swapchain, and each time a drawable frees up it copies the newest completed
//! frame into it and presents. Frames that finish between two drawables are
//! never shown, as with an unsynchronised OpenGL swap.
//!
//! The presenter copies on its **own Metal command queue**. On wgpu's queue
//! the copy into a drawable waits for the compositor to release it, and every
//! frame submitted behind that copy waited with it: measured, the renderer fell
//! from ~1,160 to ~245 frames a second, stalling once per refresh. Separate
//! queues are not ordered against each other, so the presenter waits on the
//! CPU for a published frame's GPU work before copying it, and for its own
//! copy to finish before handing the slot back.
//!
//! Slot ownership is the whole protocol. The renderer never takes the slot last
//! published (the presenter may be about to read it) or the slot the presenter
//! is copying from. Three slots therefore always leave the renderer one.
//!
//! An offscreen slot never makes the renderer wait, so acquire waits instead
//! until at most [`MAX_FRAMES_IN_FLIGHT`] published frames are unfinished on
//! the GPU, the bound a swapchain's frame latency would otherwise impose.
//! Without it the CPU queues GPU work without limit: memory grows and the
//! presenter's copy waits behind many frames.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLDevice, MTLOrigin, MTLSize,
    MTLTexture,
};
use objc2_quartz_core::CAMetalDrawable;
use wgpu::hal::api::Metal;

const SLOTS: usize = 3;
const MAX_FRAMES_IN_FLIGHT: usize = 2;

/// How long the presenter backs off when the window cannot take a frame.
const UNAVAILABLE_BACKOFF: Duration = Duration::from_millis(5);

#[derive(Debug)]
struct State {
    slots: Vec<wgpu::Texture>,
    /// The newest published slot, and the queue position after its frame.
    latest: Option<(usize, wgpu::SubmissionIndex)>,
    published: u64,
    /// The slot the presenter is copying from, if any.
    reading: Option<usize>,
    config: wgpu::SurfaceConfiguration,
    config_generation: u64,
    presented: u64,
    shutdown: bool,
    /// Queue positions just after each published frame, oldest first.
    in_flight: VecDeque<wgpu::SubmissionIndex>,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The renderer's half of mailbox presentation.
#[derive(Debug)]
pub(super) struct Mailbox {
    shared: Arc<Shared>,
    device: wgpu::Device,
    presenter: Option<JoinHandle<()>>,
}

/// Publication handle carried by a frame acquired from a mailbox slot.
#[derive(Debug)]
pub(super) struct SlotLease {
    shared: Arc<Shared>,
    slot: usize,
}

impl SlotLease {
    /// Offers the rendered slot to the presenter. Call after the frame's
    /// commands are submitted.
    pub(super) fn publish(self, queue: &wgpu::Queue) {
        // An empty submission completes after everything submitted before it.
        let done = queue.submit(std::iter::empty());
        let mut state = self.shared.lock();
        state.in_flight.push_back(done.clone());
        state.latest = Some((self.slot, done));
        state.published += 1;
        drop(state);
        self.shared.wake.notify_one();
    }
}

impl Mailbox {
    /// Starts a presenter for `surface`. `config` is the swapchain
    /// configuration; it gains `COPY_DST` because frames arrive by copy.
    pub(super) fn start(
        surface: Arc<wgpu::Surface<'static>>,
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
    ) -> Self {
        let mut config = config.clone();
        config.usage |= wgpu::TextureUsages::COPY_DST;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                slots: make_slots(device, &config),
                latest: None,
                published: 0,
                reading: None,
                config,
                config_generation: 0,
                presented: 0,
                shutdown: false,
                in_flight: VecDeque::new(),
            }),
            wake: Condvar::new(),
        });
        let presenter = {
            let shared = Arc::clone(&shared);
            let device = device.clone();
            std::thread::Builder::new()
                .name("lodestone-presenter".into())
                .spawn(move || present_loop(&surface, &device, &shared))
                .expect("spawn the presenter thread")
        };
        Self { shared, device: device.clone(), presenter: Some(presenter) }
    }

    /// A free slot to render the next frame into, with its texture.
    pub(super) fn acquire(&self) -> (wgpu::Texture, SlotLease) {
        loop {
            let oldest = {
                let mut state = self.shared.lock();
                if state.in_flight.len() < MAX_FRAMES_IN_FLIGHT {
                    break;
                }
                state.in_flight.pop_front()
            };
            if let Some(index) = oldest {
                let _ = self.device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: None });
            }
        }
        let state = self.shared.lock();
        let slot = (0..SLOTS)
            .find(|&slot| state.latest.as_ref().map(|(latest, _)| *latest) != Some(slot) && Some(slot) != state.reading)
            .expect("three slots leave one free beside the published and the presenting slot");
        let texture = state.slots[slot].clone();
        drop(state);
        (texture, SlotLease { shared: Arc::clone(&self.shared), slot })
    }

    /// Applies a new swapchain configuration; slots are rebuilt at its size.
    pub(super) fn reconfigure(&self, device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) {
        let mut config = config.clone();
        config.usage |= wgpu::TextureUsages::COPY_DST;
        let mut state = self.shared.lock();
        state.slots = make_slots(device, &config);
        state.latest = None;
        state.in_flight.clear();
        state.config = config;
        state.config_generation += 1;
    }

    /// Frames the presenter has handed to the window.
    pub(super) fn presented(&self) -> u64 {
        self.shared.lock().presented
    }

    /// Stops the presenter and waits for it, so the surface is free again.
    pub(super) fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_all();
        if let Some(presenter) = self.presenter.take() {
            let _ = presenter.join();
        }
    }
}

impl Drop for Mailbox {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn make_slots(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> Vec<wgpu::Texture> {
    (0..SLOTS)
        .map(|_| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("lodestone-render mailbox slot"),
                size: wgpu::Extent3d {
                    width: config.width.max(1),
                    height: config.height.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: config.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &config.view_formats,
            })
        })
        .collect()
}

#[expect(unsafe_code, reason = "raw Metal handles for a second command queue and the layer's drawables")]
fn present_loop(surface: &wgpu::Surface<'static>, device: &wgpu::Device, shared: &Shared) {
    // SAFETY: the raw device and layer are only used to create a queue and
    // take drawables; wgpu keeps both alive for as long as `device` and
    // `surface`, which this thread owns clones of.
    let raw_queue = unsafe { device.as_hal::<Metal>() }.and_then(|hal| hal.raw_device().newCommandQueue());
    let layer = unsafe { surface.as_hal::<Metal>() }.map(|hal| hal.render_layer().lock().clone());
    let (Some(raw_queue), Some(layer)) = (raw_queue, layer) else {
        return;
    };
    let mut shown = 0;
    let mut configured = None;
    loop {
        let pending = {
            let mut state = shared.lock();
            while !state.shutdown && state.published == shown {
                state = shared.wake.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if state.shutdown {
                return;
            }
            (configured != Some(state.config_generation))
                .then(|| (state.config.clone(), state.config_generation))
        };
        if let Some((config, generation)) = pending {
            // wgpu still configures the layer: format, size and copyability.
            // Configuring waits for the GPU to go idle, which fails while the
            // device is being torn down at exit. That must not take the
            // process down from this thread; the shutdown flag ends the loop.
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            surface.configure(device, &config);
            if pollster::block_on(scope.pop()).is_some() {
                std::thread::sleep(UNAVAILABLE_BACKOFF);
                continue;
            }
            configured = Some(generation);
        }
        autoreleasepool(|_| {
            // Blocks until the compositor frees a drawable; with the drawable
            // timeout allowed, gives up after a second instead.
            let Some(drawable) = layer.nextDrawable() else {
                std::thread::sleep(UNAVAILABLE_BACKOFF);
                return;
            };
            // Take the newest frame only now: the wait above may have lasted a
            // whole refresh interval.
            let source = {
                let mut state = shared.lock();
                match state.latest.clone() {
                    Some((slot, done)) if configured == Some(state.config_generation) => {
                        state.reading = Some(slot);
                        shown = state.published;
                        Some((state.slots[slot].clone(), done))
                    }
                    _ => None,
                }
            };
            let Some((source, done)) = source else {
                return;
            };
            let _ = device.poll(wgpu::PollType::Wait { submission_index: Some(done), timeout: None });
            copy_and_present(&raw_queue, &source, &drawable);
            let mut state = shared.lock();
            state.reading = None;
            state.presented += 1;
        });
    }
}

/// Copies `source` into `drawable` on the presenter's queue, presents it, and
/// returns once the copy has finished reading `source`.
#[expect(unsafe_code, reason = "raw Metal blit between a wgpu texture and a drawable")]
fn copy_and_present(
    queue: &ProtocolObject<dyn MTLCommandQueue>,
    source: &wgpu::Texture,
    drawable: &Retained<ProtocolObject<dyn CAMetalDrawable>>,
) {
    // SAFETY: `source` outlives this function, which waits for the copy.
    let Some(hal) = (unsafe { source.as_hal::<Metal>() }) else {
        return;
    };
    let Some(commands) = queue.commandBuffer() else {
        return;
    };
    let Some(blit) = commands.blitCommandEncoder() else {
        return;
    };
    let src = hal.raw_handle();
    let dst = drawable.texture();
    let size = MTLSize {
        width: src.width().min(dst.width()),
        height: src.height().min(dst.height()),
        depth: 1,
    };
    let origin = MTLOrigin { x: 0, y: 0, z: 0 };
    // SAFETY: both textures are alive, 2D, single-level, of the same pixel
    // format (the slots are made from the swapchain configuration), and `size`
    // fits inside both.
    unsafe {
        blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
            src, 0, 0, origin, size, &dst, 0, 0, origin,
        );
    }
    blit.endEncoding();
    commands.presentDrawable(ProtocolObject::from_ref(&**drawable));
    commands.commit();
    commands.waitUntilCompleted();
}
